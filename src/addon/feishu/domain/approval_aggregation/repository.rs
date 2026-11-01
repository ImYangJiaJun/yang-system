//! 聚合组表的唯一持久化边界。
//!
//! 组行是组实例身份（`uuid` / `instance_code` / `serial_number` / `effect_phase`）的唯一
//! 落点；leader 任务行只承载执行簿记。设计见
//! `docs/architecture/2026-10-31-feishu-approval-aggregation-design.md` §4.1、§4.2。
//!
//! # 为什么用 DSL 而不是裸 SQL
//!
//! 这里的每个动作都是「按 (config_id, group_key) 改一行/删一批」，没有行锁状态机、
//! 没有方言特性——本仓同类 CRUD 一律走 `TableQuery`（字段与能力位在构造期就校验）。
//! 裸 SQL 只在 `FOR UPDATE SKIP LOCKED` 那种构造器表达不了的场景保留（见
//! `docs/architecture/raw-sql-boundaries.md`）。
//!
//! # 事务变体
//!
//! 带 `_in_tx` 后缀的函数在调用方事务内执行，供「解散/建组/收尾」这类必须与其它写
//! 原子提交的组合使用；其余读函数直接走连接池。
//!
//! 组维护的租约判定必须与写同事务：`lock_*_in_tx` 用 `SELECT ... FOR UPDATE` 锁住
//! 任务行，并在行锁内复检 `worker_id`/`lease_until`/`state`/`effect_phase`——连接池
//! 普通读与随后的 UPDATE/DELETE 之间，`claim_ready` 可以把该行认领走（设计 §6.1）。

use serde_json::Value;
use yang_base::table::Record;
use yang_db::Transaction;

use super::super::context::FeishuContext;

/// 锁住配置行（与 `admit` / `delete_config` 同一把 `FOR UPDATE` 行锁），返回配置是
/// 否仍然启用。
///
/// # 为什么要锁配置行
///
/// 播种的解散/建组必须与「受理插任务行」「删除配置清任务行」互斥，否则会出现
/// 「播种刚判定组可解散，受理同时给该组插了一行」这类交错。三处按同一顺序
/// （先配置行、后任务行）加锁，不会成环。
///
/// 用 `yang_db::QueryBuilder` 的行锁入口而不是裸 SQL：这里只有一行按主键取锁，
/// 不需要方言逃生口（见模块文档对裸 SQL 的取舍）。
pub(crate) async fn lock_config_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
) -> anyhow::Result<bool> {
    let rows = transaction
        .select_for_update::<Record>(
            yang_db::QueryBuilder::from_pool(
                context.approval_configs().pool(),
                yang_db::table!("feishu_approval_config"),
            )
            .fields(&[yang_db::field!("id"), yang_db::field!("enabled")])
            .where_and(yang_db::field!("id"), yang_db::CompareOp::Eq, config_id)
            .map_err(anyhow::Error::from)?,
        )
        .await?;
    Ok(rows
        .first()
        .and_then(|row| row.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

/// 读取行快照里的服务端当前时间（`select_expr(UNIX_TIMESTAMP(), field!("now"))` 的别名）。
///
/// 不走 `Record::require("now")`：`now` 是查询别名、不是表列，而 schema_anchor 会把
/// `Record` 访问的键按表声明对账——那是它该做的事，所以这里用 `get` 显式绕开。
pub(crate) fn server_now(row: &Record) -> anyhow::Result<i64> {
    row.get("now")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("任务行快照缺少服务端时间列 now"))
}

/// 任务行是否仍被某个实例持有（有效租约）。
///
/// `now` 必须来自数据库（行快照里的 `now` 列，由 `UNIX_TIMESTAMP()` 生成）：租约是
/// 用数据库时钟写入的，本地时钟与数据库有偏差时会把有效租约误判成过期。
///
/// 有持有者但没有到期时刻时按「仍持有」处理，宁可本轮不动，等下一轮再判。
pub(crate) fn has_live_lease(row: &Record, now: i64) -> bool {
    let holder = row
        .optional::<String>("worker_id")
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty());
    let lease = row.optional::<i64>("lease_until").ok().flatten();
    match (holder, lease) {
        (Some(_), None) => true,
        (Some(_), Some(until)) => until > now,
        _ => false,
    }
}

/// 可被「重新点击」复位为 `pending` 的状态白名单。
///
/// 与单条 [`super::super::approval_execution::reopen`] 的 SQL 同口径，另加组级
/// `paused`：暂停组只能靠人工批量点击重试。
pub(crate) fn reopenable_state(state: &str) -> bool {
    matches!(
        state,
        "pending" | "review" | "terminal" | "unnotified" | "backfilled" | "paused"
    )
}

/// 可以随组一起删除的状态：这些状态都没有绑定外部事实。
///
/// `created` / `backfilled` 与实例码、编号绑定，只能摘组保留；`creating` 表示外部
/// 写入进行中，任何情况下都不删（设计 §6.1 的 `state`/`effect_phase` 条件）。
pub(crate) fn dissolvable_state(state: &str) -> bool {
    matches!(
        state,
        "pending" | "review" | "terminal" | "unnotified" | "paused"
    )
}

/// 把组 leader 复位为 `pending`，并把任务行阶段设成 `leader_phase`。
///
/// **组行阶段不动**：组 `effect_phase` 是外部副作用的唯一事实，`reopen` 只负责把
/// leader 送回可认领集合（设计 §4.1、§4.6）。
///
/// 与单条 [`super::super::approval_execution::reopen`] 的差别只有两点：不按
/// `instance_code` 派生阶段（leader 永远不持有组实例码），以及不改
/// `last_error`（通知去重依赖它）。
///
/// # 行锁内复检
///
/// 调用方的「无租约」判断来自事务外读，`claim_ready` 又不锁配置行；两读之间另一
/// 实例可以把该行认领走。无条件 UPDATE 会清掉有效租约，在途处理的 `is_owned` /
/// `finish` 随即失败。这里先 `FOR UPDATE` 锁住任务行、复检状态白名单与租约，
/// 再按 id 更新——复检通过后到提交前，行不会被认领方改写（设计 §6.1）。
pub(crate) async fn reopen_group_leader_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    task_id: i64,
    leader_phase: &str,
) -> anyhow::Result<bool> {
    let locked = lock_task_rows_in_tx(transaction, context, &[task_id], false)
        .await?
        .unwrap_or_default();
    let Some(current) = locked.first() else {
        return Ok(false);
    };
    let now = server_now(current)?;
    let state: String = current.require("state")?;
    if !reopenable_state(&state) || has_live_lease(current, now) {
        return Ok(false);
    }
    let row = Record::new()
        .set("state", "pending")
        .set("effect_phase", leader_phase)
        .set("worker_id", Value::Null)
        .set("lease_until", Value::Null)
        .set("attempts", 0)
        .set("available_at", Value::Null);
    let updated = context
        .approval_tasks()
        .query()
        .where_eq("id", serde_json::json!(task_id))?
        .update_in_tx(transaction, row)
        .await?;
    Ok(updated == 1)
}

/// 插入一行组内任务行：`group_seq=0` 是 leader，`>=1` 是成员。
///
/// `uuid` 仍按成员自己的 `record_id` 派生（服务单条点击的幂等），组 uuid 只落组行。
pub(crate) async fn insert_group_task_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    group_seq: i64,
    record_id: &str,
    uuid: &str,
) -> anyhow::Result<()> {
    let row = Record::new()
        .set("config_id", config_id)
        .set("record_id", record_id)
        .set("uuid", uuid)
        .set("state", "pending")
        .set("group_id", group_key)
        .set("group_seq", group_seq);
    context
        .approval_tasks()
        .query()
        .insert_in_tx(transaction, row)
        .await?;
    Ok(())
}

/// 把一行任务摘出组，保留为单条任务（`group_id=NULL`、`group_seq=0`）。
///
/// 解散时用于「组内已有自身实例码或回填」的行：那些行的外部事实属于它自己，
/// 不能随组一起删掉。
///
/// 行先在当前事务内 `FOR UPDATE` 锁住并复检租约：正在执行的成员不摘组，否则
/// 在途处理看到的成员集合会在中途变化。返回 `false` 表示这一行没有动。
pub(crate) async fn detach_task_from_group_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    task_id: i64,
) -> anyhow::Result<bool> {
    let locked = lock_task_rows_in_tx(transaction, context, &[task_id], false)
        .await?
        .unwrap_or_default();
    let Some(current) = locked.first() else {
        return Ok(false);
    };
    let now = server_now(current)?;
    if has_live_lease(current, now) {
        return Ok(false);
    }
    let row = Record::new()
        .set("group_id", Value::Null)
        .set("group_seq", 0);
    let updated = context
        .approval_tasks()
        .query()
        .where_eq("id", serde_json::json!(task_id))?
        .update_in_tx(transaction, row)
        .await?;
    Ok(updated == 1)
}

/// `where_in` 的单次元素上限（与框架 `MAX_IN_LIST_SIZE` 同值）。
///
/// 框架对超限是**拒绝**而不是截断，所以批量删除必须自己按批切分。
const IN_CHUNK: usize = 500;

/// 插入一行组行（阶段取表声明的默认值 `ready`）。
///
/// 组键是幂等键：重复播种会撞 `(config_id, group_key)` 唯一索引，错误原样上抛——
/// 调用方据此判断「这组已经存在」。
pub(crate) async fn insert_group_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    signature_hash: &str,
    uuid: &str,
) -> anyhow::Result<()> {
    let row = Record::new()
        .set("config_id", config_id)
        .set("group_key", group_key)
        .set("signature_hash", signature_hash)
        .set("uuid", uuid);
    context
        .approval_groups()
        .query()
        .insert_in_tx(transaction, row)
        .await?;
    Ok(())
}

/// 记下「组实例已创建」：实例码与阶段 `created` 写组行。
///
/// **实例码只写组行**：leader 任务行的 `instance_code` 保持为 NULL（设计 §4.6 第 7 步）。
pub(crate) async fn save_created_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    instance_code: &str,
) -> anyhow::Result<()> {
    let row = Record::new()
        .set("instance_code", instance_code)
        .set("effect_phase", "created");
    context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_key", serde_json::json!(group_key))?
        .update_in_tx(transaction, row)
        .await?;
    Ok(())
}

/// 记下组编号（回填成功后调用）。
///
/// 只写 `serial_number`：阶段由 `save_created_in_tx` / [`mark_group_phase_in_tx`] 负责，
/// 这里不重复裁定。
pub(crate) async fn save_backfilled_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    serial_number: &str,
) -> anyhow::Result<()> {
    let row = Record::new().set("serial_number", serial_number);
    context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_key", serde_json::json!(group_key))?
        .update_in_tx(transaction, row)
        .await?;
    Ok(())
}

/// 改写组行的外部写入阶段；`instance` 非空时一并落实例码。
///
/// `ready` / `creating` / `lookup` / `created` 的推进与「按 uuid 回捞后补实例码」都走
/// 这一个入口：组行阶段是**唯一**的外部副作用事实，`reopen` 不会重置它。
pub(crate) async fn mark_group_phase_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    phase: &str,
    instance: Option<&str>,
) -> anyhow::Result<()> {
    let mut row = Record::new().set("effect_phase", phase);
    if let Some(instance) = instance {
        row = row.set("instance_code", instance);
    }
    context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_key", serde_json::json!(group_key))?
        .update_in_tx(transaction, row)
        .await?;
    Ok(())
}

/// 删除一行组行，返回影响行数（0 表示组不存在）。
pub(crate) async fn delete_group_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
) -> anyhow::Result<u64> {
    let deleted = context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_key", serde_json::json!(group_key))?
        .delete_in_tx(transaction)
        .await?;
    Ok(deleted)
}

/// 删除某配置的全部组行，返回影响行数（`delete_config` 同事务清理）。
pub(crate) async fn delete_groups_for_config_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
) -> anyhow::Result<u64> {
    let deleted = context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .delete_in_tx(transaction)
        .await?;
    Ok(deleted)
}

/// 按 (配置, 组键) 读组行。
pub(crate) async fn load_group(
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
) -> anyhow::Result<Option<Record>> {
    let row = context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_key", serde_json::json!(group_key))?
        .optional()
        .await?;
    Ok(row)
}

/// 按实例编码读组行：事件 worker **先查组表**，避免 leader 行截获组实例的事件。
pub(crate) async fn load_group_by_instance(
    context: &FeishuContext,
    instance_code: &str,
) -> anyhow::Result<Option<Record>> {
    let row = context
        .approval_groups()
        .query()
        .where_eq("instance_code", serde_json::json!(instance_code))?
        .optional()
        .await?;
    Ok(row)
}

/// 按组 `uuid` 读组行：`60012` 响应丢失时唯一的对账入口。
pub(crate) async fn load_group_by_uuid(
    context: &FeishuContext,
    uuid: &str,
) -> anyhow::Result<Option<Record>> {
    let row = context
        .approval_groups()
        .query()
        .where_eq("uuid", serde_json::json!(uuid))?
        .optional()
        .await?;
    Ok(row)
}

/// 读某配置的全部组行（组维护逐组判断）。
pub(crate) async fn list_groups_for_config(
    context: &FeishuContext,
    config_id: i64,
) -> anyhow::Result<Vec<Record>> {
    let rows = context
        .approval_groups()
        .query()
        .where_eq("config_id", serde_json::json!(config_id))?
        .all()
        .await?;
    Ok(rows)
}

/// 把「已建实例」组里一条记录已不存在的成员行摘出组并置 `terminal`。
///
/// 设计 §5 的 Fatal 分支：组行**已有实例码**时，记录被删/无权限的成员不能再参与
/// 回填（写它必然失败，还会把整组拖进重试预算），把它摘出并落终态，其余成员继续
/// 用同一编号回填；否则整组会因为一条毒记录永久停在可重试。
///
/// # 只摘成员，不摘 leader
///
/// `finish_group` 按 `group_id + group_seq=0` 定位组，leader 摘掉就再也收不了尾。
/// 调用方必须先判断「这一行不是本次持有的 leader」；leader 记录不可读时整组暂停。
pub(crate) async fn detach_member_as_terminal(
    context: &FeishuContext,
    task_id: i64,
    reason: &str,
) -> anyhow::Result<bool> {
    let row = Record::new()
        .set("state", "terminal")
        .set("last_error", reason)
        .set("group_id", Value::Null)
        .set("group_seq", 0)
        .set("worker_id", Value::Null)
        .set("lease_until", Value::Null);
    let updated = context
        .approval_tasks()
        .query()
        .where_eq("id", serde_json::json!(task_id))?
        .update(row)
        .await?;
    Ok(updated == 1)
}

/// 读一个组的全部任务行（按 `group_seq` 升序），供处理期取成员与明细行序。
///
/// 与 [`lock_group_members_in_tx`] 的区别只在**不锁**：处理期已经持有 leader 的
/// 执行租约，成员集合在这里只用于组装表单与回填，不参与「可不可以写」的判定
/// （那由 `finish_group` / 认领 SQL 自己保证）。
pub(crate) async fn load_members(
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
) -> anyhow::Result<Vec<Record>> {
    let rows = context
        .approval_tasks()
        .query()
        .select_fields(&[
            "id",
            "record_id",
            "group_seq",
            "state",
            "effect_phase",
            "instance_code",
            "serial_number",
        ])?
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_id", serde_json::json!(group_key))?
        .order_by("group_seq", yang_base::table::SortOrder::Asc)?
        .order_by("id", yang_base::table::SortOrder::Asc)?
        .all()
        .await?;
    Ok(rows)
}

/// 在调用方事务内锁住一个组的**全部任务行**（`FOR UPDATE`，按 `group_seq` 升序）。
///
/// # 为什么必须锁而不只是读
///
/// 调用方基于成员行判定「leader 无租约、可以复位/解散」，随后还要按 id 更新/删除。
/// `claim_ready` 只锁任务行、不锁配置行，连接池普通读到「无租约」与写之间足以让
/// 另一实例认领该行：复位会清掉有效租约，删除会删掉在途任务行（设计 §6.1）。
/// 行锁把「判定」与「写」放进同一个事务窗口，认领方只能跳过或在提交后重读。
///
/// 快照带 `now` 列（服务端 `UNIX_TIMESTAMP()`）：租约判定必须用数据库时钟，
/// 与认领写入的租约同源。
///
/// # 为什么 leader 也在返回里
///
/// leader 的那条记录同样是组内业务记录：明细每条成员一行（含 leader 的记录）、
/// 回填给「全部成员」写同一编号、解散判据要「全组都在本次勾选内」——三处都不能少
/// 了这一行。调用方按 `group_seq` 区分角色（0 = leader，`>=1` = 成员）。
///
/// 顺序即明细行序——播种按扫描顺序落 `group_seq`，所以「组内第几行」是稳定的。
pub(crate) async fn lock_group_members_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
) -> anyhow::Result<Vec<Record>> {
    let rows = transaction
        .select_for_update::<Record>(
            yang_db::QueryBuilder::from_pool(
                context.approval_tasks().pool(),
                yang_db::table!("feishu_approval_task"),
            )
            .fields(&[
                yang_db::field!("id"),
                yang_db::field!("record_id"),
                yang_db::field!("group_seq"),
                yang_db::field!("state"),
                yang_db::field!("effect_phase"),
                yang_db::field!("instance_code"),
                yang_db::field!("serial_number"),
                yang_db::field!("worker_id"),
                yang_db::field!("lease_until"),
            ])
            .select_expr(yang_db::SqlExpr::unix_timestamp(), yang_db::field!("now"))
            .where_and(
                yang_db::field!("config_id"),
                yang_db::CompareOp::Eq,
                config_id,
            )
            .map_err(anyhow::Error::from)?
            .where_and(
                yang_db::field!("group_id"),
                yang_db::CompareOp::Eq,
                group_key,
            )
            .map_err(anyhow::Error::from)?
            .order(yang_db::field!("group_seq"), yang_db::SortOrder::Asc)
            .order(yang_db::field!("id"), yang_db::SortOrder::Asc),
        )
        .await?;
    Ok(rows)
}

/// 把组内**成员行**（`group_seq > 0`）改成同一状态，返回受影响行数。
///
/// # leader 不在射程内
///
/// leader 的状态由 `finish_group` / 处理期单独裁定（它同时管租约与认领）。**暂停组时
/// 调用方必须自己把 leader 也置为 `paused`**——只暂停成员会让 leader 下一轮被认领，
/// 组的暂停就形同虚设。
///
/// # 为什么不直接 `where_gt("group_seq", 0)`
///
/// `group_seq` 只开了 `sortable`（成员行的组内顺序依赖它），`filterable` 没开，
/// DSL 是 fail-closed：拿它当筛选条件会在运行期报 403。同一事务里先读出成员 id、
/// 再按 `id`（filterable）批量更新，既不动 schema，也保持原子性。
///
/// `reason` 为空时不动 `last_error`：`last_error` 是通知去重的判据，清空会让同一结论
/// 每轮重发一条。
pub(crate) async fn set_members_state_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    config_id: i64,
    group_key: &str,
    state: &str,
    reason: Option<&str>,
) -> anyhow::Result<u64> {
    let rows = context
        .approval_tasks()
        .query()
        .select_fields(&["id", "group_seq"])?
        .where_eq("config_id", serde_json::json!(config_id))?
        .where_eq("group_id", serde_json::json!(group_key))?
        .all_in_tx(transaction)
        .await?;
    let mut member_ids: Vec<Value> = Vec::with_capacity(rows.len());
    for row in &rows {
        let seq: i64 = row.require("group_seq")?;
        if seq > 0 {
            member_ids.push(serde_json::json!(row.require::<i64>("id")?));
        }
    }
    if member_ids.is_empty() {
        return Ok(0);
    }
    let mut row = Record::new().set("state", state);
    if let Some(reason) = reason {
        row = row.set("last_error", reason);
    }
    let mut updated = 0u64;
    for chunk in member_ids.chunks(IN_CHUNK) {
        updated += context
            .approval_tasks()
            .query()
            .where_in("id", chunk.to_vec())?
            .update_in_tx(transaction, row.clone())
            .await?;
    }
    Ok(updated)
}

/// 在调用方事务内按 id 锁住任务行（`FOR UPDATE`），附服务端当前时间 `now`。
///
/// `require_deletable` 为真时顺带复检「可以随组删除」：行数必须与请求一致，且每行
/// 都未被持有、`effect_phase='ready'`、状态不绑定外部事实。返回 `None` 表示整批
/// 不可删（调用方一行都不动）。
async fn lock_task_rows_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    task_ids: &[i64],
    require_deletable: bool,
) -> anyhow::Result<Option<Vec<Record>>> {
    let rows = transaction
        .select_for_update::<Record>(
            yang_db::QueryBuilder::from_pool(
                context.approval_tasks().pool(),
                yang_db::table!("feishu_approval_task"),
            )
            .fields(&[
                yang_db::field!("id"),
                yang_db::field!("state"),
                yang_db::field!("effect_phase"),
                yang_db::field!("worker_id"),
                yang_db::field!("lease_until"),
            ])
            .select_expr(yang_db::SqlExpr::unix_timestamp(), yang_db::field!("now"))
            .where_in(yang_db::field!("id"), task_ids.to_vec()),
        )
        .await?;
    if !require_deletable {
        return Ok(Some(rows));
    }
    if rows.len() != task_ids.len() {
        return Ok(None);
    }
    for row in &rows {
        let now = server_now(row)?;
        let state: String = row.require("state")?;
        let phase: String = row.require("effect_phase")?;
        if has_live_lease(row, now) || phase != "ready" || !dissolvable_state(&state) {
            return Ok(None);
        }
    }
    Ok(Some(rows))
}

/// 删除任务行：先删 `feishu_approval_task_applicant`，再删任务行。
///
/// # 顺序不能反
///
/// applicant 表没有外键，先删任务行会留下指向已删除任务的孤儿 PII（与 `delete_config`
/// 同一口径）。两段都按 `IN_CHUNK` 切分：框架对超限的 `IN` 是拒绝，不是截断。
///
/// # 全有或全无
///
/// 组行与成员行必须一起消失，部分删除会留下指向空组的行。所以先 `FOR UPDATE` 锁住
/// 整批行并复检：任何一行仍在执行（有效租约）、已进入外部写入阶段（`effect_phase`
/// 非 `ready`）、状态绑定外部事实或行已不存在，就**一行都不删**、返回 0——调用方据此
/// 放弃本次解散（设计 §6.1）。
pub(crate) async fn delete_task_rows_in_tx(
    transaction: &mut Transaction,
    context: &FeishuContext,
    task_ids: &[i64],
) -> anyhow::Result<u64> {
    if task_ids.is_empty() {
        return Ok(0);
    }
    for chunk in task_ids.chunks(IN_CHUNK) {
        if lock_task_rows_in_tx(transaction, context, chunk, true)
            .await?
            .is_none()
        {
            return Ok(0);
        }
    }
    let mut deleted = 0u64;
    for chunk in task_ids.chunks(IN_CHUNK) {
        let values: Vec<Value> = chunk.iter().map(|id| serde_json::json!(id)).collect();
        context
            .approval_task_applicants()
            .query()
            .where_in("task_id", values.clone())?
            .delete_in_tx(transaction)
            .await?;
        deleted += context
            .approval_tasks()
            .query()
            .where_in("id", values)?
            .delete_in_tx(transaction)
            .await?;
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 组行与任务行的列名必须与表声明一致：`Record::new().set(..)` 的键是字符串，
    /// 拼错不会在编译期报错，只会在运行期被字段校验拒掉。
    #[test]
    fn every_column_used_here_is_declared_on_its_table() {
        let group = crate::addon::feishu::approval::domain::group_table::table_spec()
            .and_then(|spec| spec.table_definition());
        let group = group.unwrap_or_else(|error| panic!("组表声明应有效: {error}"));
        for name in [
            "config_id",
            "group_key",
            "signature_hash",
            "uuid",
            "instance_code",
            "effect_phase",
            "serial_number",
        ] {
            assert!(group.field(name).is_some(), "组表缺少列 {name}");
        }

        let task = crate::addon::feishu::approval::domain::task_table::table_spec()
            .and_then(|spec| spec.table_definition());
        let task = task.unwrap_or_else(|error| panic!("任务表声明应有效: {error}"));
        for name in [
            "id",
            "config_id",
            "group_id",
            "group_seq",
            "state",
            "last_error",
        ] {
            assert!(task.field(name).is_some(), "任务表缺少列 {name}");
        }
        let group_seq = task
            .field("group_seq")
            .unwrap_or_else(|| panic!("group_seq 字段必须存在"));
        assert!(group_seq.is_sortable(), "组维护按 group_seq 排序依赖该位");
        let group_id = task
            .field("group_id")
            .unwrap_or_else(|| panic!("group_id 字段必须存在"));
        assert!(
            group_id.is_filterable(),
            "按组键定位成员依赖该位（fail-closed）"
        );
    }

    /// `IN_CHUNK` 必须与框架上限一致：写大了框架直接拒（`ParamInvalid`），
    /// 写小了只是多几轮，但「以为能一次传 1000 个」的那次会整条查询失败。
    #[test]
    fn in_chunk_matches_the_framework_limit() {
        assert_eq!(IN_CHUNK, 500);
    }
}
