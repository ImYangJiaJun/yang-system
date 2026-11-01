//! 审批任务的跨进程执行权。租约只约束本地写者，不构成飞书侧 fencing。

//! raw-sql-boundary: domain-repository feishu-approval-execution
use super::super::context::FeishuContext;
use super::DispatchSnapshot;
use yang_base::table::Record;

const LEASE_SECONDS: i64 = 300;

/// 受理与身份快照同事务。已存在任务的第一份快照胜出，不能覆盖正在执行的申请人。
pub(crate) async fn admit(
    context: &FeishuContext,
    snapshot: &DispatchSnapshot,
    record_id: &str,
) -> anyhow::Result<i64> {
    let config: Record = snapshot.config.clone().into();
    let config_id: i64 = config.require("id")?;
    let base: String = config.require("base_token")?;
    let table: String = config.require("table_id")?;
    let approval: String = config.require("approval_code")?;
    let uuid = super::super::approval_uuid::derive_uuid(&base, &table, &approval, record_id);
    let payload = serde_json::to_string(snapshot)?;
    let pool = context.approval_tasks().pool();
    let mut transaction = pool.begin().await?;
    // tenant-boundary: raw-sql feishu-approval-admit-config
    let enabled = sqlx::query_scalar::<_, bool>(
        "SELECT enabled FROM feishu_approval_config WHERE id=? FOR UPDATE",
    )
    .bind(config_id)
    .fetch_optional(&mut *transaction)
    .await?;
    anyhow::ensure!(enabled == Some(true), "审批配置已删除或停用，未受理");
    // tenant-boundary: raw-sql feishu-approval-admit-task
    sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,dispatch_payload,created_at,updated_at) VALUES (?,?,?,'pending',?,UNIX_TIMESTAMP(),UNIX_TIMESTAMP()) ON DUPLICATE KEY UPDATE id=LAST_INSERT_ID(id)")
        .bind(config_id).bind(record_id).bind(&uuid).bind(&payload).execute(&mut *transaction).await?;
    // tenant-boundary: raw-sql feishu-approval-admit-owner
    let (id, stored_config, stored_uuid): (i64, i64, String) = sqlx::query_as(
        "SELECT id,config_id,uuid FROM feishu_approval_task WHERE config_id=? AND record_id=? FOR UPDATE",
    )
    .bind(config_id)
    .bind(record_id)
    .fetch_one(&mut *transaction)
    .await?;
    anyhow::ensure!(
        stored_config == config_id && stored_uuid == uuid,
        "该记录归属另一审批配置，拒绝复用任务"
    );
    // 快照只在缺席时补写（`COALESCE` 保证第一份胜出）。不再要求「未认领且
    // ready」：review/终态的任务被重新点击时也要带回受理快照（申请人/接收人），
    // 已有快照绝不会被覆盖。
    // tenant-boundary: raw-sql feishu-approval-admit-snapshot
    sqlx::query(
        "UPDATE feishu_approval_task SET dispatch_payload=COALESCE(dispatch_payload,?) WHERE id=?",
    )
    .bind(&payload)
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    // 身份跟着**存下来的**快照走，不是跟着本次请求：`COALESCE` 让首份快照胜出，
    // 后来的点击不能替别人的任务改发起人。撞唯一索引即「已有身份」，不再写。
    let stored_payload: Option<String> =
        // tenant-boundary: raw-sql feishu-approval-admit-identity-snapshot
        sqlx::query_scalar("SELECT dispatch_payload FROM feishu_approval_task WHERE id=?")
            .bind(id)
            .fetch_one(&mut *transaction)
            .await?;
    let persisted = stored_payload
        .as_deref()
        .map(serde_json::from_str::<DispatchSnapshot>)
        .transpose()?;
    if let Some(applicant) = persisted
        .as_ref()
        .and_then(|snapshot| snapshot.applicant.as_deref())
    {
        // tenant-boundary: raw-sql feishu-approval-admit-applicant
        sqlx::query("INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?,?) ON DUPLICATE KEY UPDATE id=id")
            .bind(id).bind(applicant).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(id)
}

/// 任务行存在但**没有身份**：把本次点击人补进去（有身份时什么都不动）。
///
/// # 为什么需要它
///
/// 发起人只能来自点击人。`admit` 的快照是「首次受理胜出」，而早于身份落库机制的
/// 存量任务行存着一份 `applicant: null` 的快照——它们认领后会被判「没有发起人」、
/// 零出站地空转，再点一次也救不回来（`COALESCE` 永远保留旧快照）。本函数在
/// **身份缺席**时补写，这条行就自愈了。
///
/// 已有身份时一列都不动：后来的请求不能替别人的任务改发起人，即使点击人是同事
/// ——与 `admit` 的首次快照语义同一条不变量。
pub(crate) async fn attach_initiator(
    context: &FeishuContext,
    task_id: i64,
    snapshot: &DispatchSnapshot,
) -> anyhow::Result<()> {
    let Some(applicant) = snapshot.applicant.as_deref() else {
        return Ok(());
    };
    let payload = serde_json::to_string(snapshot)?;
    let pool = context.approval_tasks().pool();
    let mut transaction = pool.begin().await?;
    // tenant-boundary: raw-sql feishu-approval-attach-initiator-read
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT dispatch_payload FROM feishu_approval_task WHERE id=? FOR UPDATE",
    )
    .bind(task_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let existing: Option<String> =
        // tenant-boundary: raw-sql feishu-approval-attach-initiator-existing
        sqlx::query_scalar("SELECT open_id FROM feishu_approval_task_applicant WHERE task_id=?")
            .bind(task_id)
            .fetch_optional(&mut *transaction)
            .await?;
    let from_snapshot = stored
        .as_deref()
        .and_then(|payload| serde_json::from_str::<DispatchSnapshot>(payload).ok())
        .and_then(|snapshot| snapshot.applicant)
        .filter(|applicant| !applicant.trim().is_empty());
    let from_table = existing.filter(|open_id| !open_id.trim().is_empty());
    if from_snapshot.is_some() || from_table.is_some() {
        transaction.commit().await?;
        return Ok(());
    }
    // tenant-boundary: raw-sql feishu-approval-attach-initiator-write
    sqlx::query(
        "UPDATE feishu_approval_task SET dispatch_payload=?, updated_at=UNIX_TIMESTAMP() WHERE id=?",
    )
    .bind(&payload)
    .bind(task_id)
    .execute(&mut *transaction)
    .await?;
    // tenant-boundary: raw-sql feishu-approval-attach-initiator-row
    sqlx::query("INSERT INTO feishu_approval_task_applicant (task_id,open_id) VALUES (?,?) ON DUPLICATE KEY UPDATE open_id=VALUES(open_id)")
        .bind(task_id)
        .bind(applicant)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// 把「重新点击」变成一次人工重试：任务不在执行中、又停在可重试状态时，
/// 重置回 `pending` 并让调用方立刻重新认领。
///
/// # 为什么安全
///
/// 任务行与 `uuid` 都不动——`uuid` 由 `base/table/approval_code/record_id`
/// 确定性派生，是飞书创建接口的幂等键：若上次其实建出了实例，重新 create
/// 会拿到 `60012` 并走按 uuid 回捞，不会产生第二条审批。
///
/// # 收窄条件
///
/// - `pending / review / terminal / unnotified / paused` 可重开。`paused` 是聚合组
///   整组暂停后的人工重试入口：暂停组不参与轮询认领，只有重新点击才能拉起。
/// - `backfilled` **也能重开，但只由调用方在「人工点击且源记录回填列为空」时**调用
///   ——那是用户清空编号之后的显式重发请求。重开后 `effect_phase` 因实例已存在
///   变为 `created`，`dispatch_one` 走「按 instance_code / uuid 恢复实例并重新
///   回填」，不新建审批；uuid 幂等兜底。（单条点击若回填列非空则不调用本函数，
///   保持原有 Queued 行为。）
/// - 有执行者持有有效租约时不动（条件 UPDATE 兜底），避免把正在写入的任务
///   抢回来并发执行。
/// - 顺带清退避（`available_at`）与重试计数：点击的语义就是「现在再来一次」。
/// - **保留 `last_error`**：它是通知去重的判据（`should_notify` 比较「本轮原因 ==
///   上一轮原因」）。重开时清空会让同一结论每点一次都重发一条通知——2026-10-28
///   真机：批量点击反复收到另一张表的旧 waiting 报错。
pub(crate) async fn reopen(context: &FeishuContext, task_id: i64) -> anyhow::Result<bool> {
    // tenant-boundary: raw-sql feishu-approval-reopen
    let result = sqlx::query(
        "UPDATE feishu_approval_task SET state='pending', effect_phase=IF(instance_code IS NULL,'ready','created'), worker_id=NULL, lease_until=NULL, attempts=0, available_at=NULL, updated_at=UNIX_TIMESTAMP() WHERE id=? AND state IN ('pending','review','terminal','unnotified','backfilled','paused') AND (worker_id IS NULL OR lease_until<=UNIX_TIMESTAMP())",
    )
    .bind(task_id)
    .execute(context.approval_tasks().pool())
    .await?;
    Ok(result.rows_affected() > 0)
}

pub(crate) async fn facts(
    context: &FeishuContext,
    claim: &TaskClaim,
) -> anyhow::Result<(String, Option<String>)> {
    // tenant-boundary: raw-sql feishu-approval-effect-facts
    let row = sqlx::query_as("SELECT effect_phase,instance_code FROM feishu_approval_task WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review'")
        .bind(claim.id).bind(&claim.owner).bind(claim.generation).fetch_optional(context.approval_tasks().pool()).await?;
    row.ok_or_else(|| anyhow::anyhow!("审批任务执行权已失效"))
}

pub(crate) async fn begin_effect(
    context: &FeishuContext,
    claim: &TaskClaim,
    phase: &str,
) -> anyhow::Result<()> {
    // tenant-boundary: raw-sql feishu-approval-effect-begin
    let result = sqlx::query("UPDATE feishu_approval_task SET effect_phase=?,lease_until=UNIX_TIMESTAMP()+?,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND effect_phase IN ('ready','created')")
        .bind(phase).bind(LEASE_SECONDS).bind(claim.id).bind(&claim.owner).bind(claim.generation).execute(context.approval_tasks().pool()).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "外部写入未开始：执行权失效或上次写入结果未知"
    );
    Ok(())
}

pub(crate) async fn confirm_effect(
    context: &FeishuContext,
    claim: &TaskClaim,
    phase: &str,
    instance: Option<&str>,
) -> anyhow::Result<()> {
    // tenant-boundary: raw-sql feishu-approval-effect-confirm
    let result = sqlx::query("UPDATE feishu_approval_task SET effect_phase=?,instance_code=COALESCE(?,instance_code),updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review'")
        .bind(phase).bind(instance).bind(claim.id).bind(&claim.owner).bind(claim.generation).execute(context.approval_tasks().pool()).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "远端结果未能持久确认，禁止重放写入"
    );
    Ok(())
}

pub(crate) async fn mark_unknown(context: &FeishuContext, claim: &TaskClaim) -> anyhow::Result<()> {
    // tenant-boundary: raw-sql feishu-approval-effect-unknown
    sqlx::query("UPDATE feishu_approval_task SET state='review',last_error='外部写入结果未知，禁止自动重放；请人工核查',updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=?")
        .bind(claim.id).bind(&claim.owner).bind(claim.generation).execute(context.approval_tasks().pool()).await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct TaskClaim {
    pub(crate) id: i64,
    pub(crate) owner: String,
    pub(crate) generation: i64,
    pub(crate) attempts: i64,
}

/// 原子认领可安全执行的任务；只读阶段租约过期允许接管，未知副作用禁止重放。
///
/// # 聚合组的认领口径
///
/// - 批量（`only_id=None`）只认组 leader（`group_seq=0`）：成员行只承载自身状态，
///   执行副作用绑定在 leader 上；按成员认领会让同一组被处理多遍。
/// - 单条（`only_id=Some`）不受组过滤限制：单条点击先摘组再认领，这里只是兜底。
/// - 组 leader 的 `lookup` 阶段可认领：`60012` 之后要按组 uuid 回捞，没有这次
///   认领就永远等不到（设计 §4.5）。
///
/// 过期租约清扫同样排除组 leader 的 `lookup`：清扫先把「阶段不是
/// `ready`/`created`」的行写成 `review`，若不排除，回捞阶段会在认领前先被扫成人工
/// 核查，组永久停摆（设计 §4.6 第 12 步）。
pub(crate) async fn claim_ready(
    context: &FeishuContext,
    after: i64,
    only_id: Option<i64>,
    limit: usize,
) -> anyhow::Result<Vec<TaskClaim>> {
    let pool = context.approval_tasks().pool();
    // tenant-boundary: raw-sql feishu-approval-execution-1
    sqlx::query("UPDATE feishu_approval_task SET state='review',last_error='外部写入结果未知，禁止自动重放；请人工核查',updated_at=UNIX_TIMESTAMP() WHERE worker_id IS NOT NULL AND lease_until<=UNIX_TIMESTAMP() AND effect_phase NOT IN ('ready','created') AND NOT (group_id IS NOT NULL AND group_seq=0 AND effect_phase='lookup') AND state<>'review'")
        .execute(pool).await?;
    let mut transaction = pool.begin().await?;
    // tenant-boundary: raw-sql feishu-approval-execution-2
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as("SELECT id,generation,attempts FROM feishu_approval_task WHERE id>? AND (? IS NULL OR id=?) AND state='pending' AND (effect_phase IN ('ready','created') OR (group_id IS NOT NULL AND group_seq=0 AND effect_phase='lookup')) AND (? IS NOT NULL OR group_seq=0) AND (available_at IS NULL OR available_at<=UNIX_TIMESTAMP()) AND (worker_id IS NULL OR lease_until<=UNIX_TIMESTAMP()) ORDER BY id LIMIT ? FOR UPDATE SKIP LOCKED")
        .bind(after).bind(only_id).bind(only_id).bind(only_id).bind(limit as u32)
        .fetch_all(&mut *transaction).await?;
    let mut claims = Vec::with_capacity(rows.len());
    for (id, generation, attempts) in rows {
        let owner = uuid::Uuid::new_v4().to_string();
        // tenant-boundary: raw-sql feishu-approval-execution-3
        sqlx::query("UPDATE feishu_approval_task SET worker_id=?,generation=generation+1,lease_until=UNIX_TIMESTAMP()+?,updated_at=UNIX_TIMESTAMP() WHERE id=?")
            .bind(&owner).bind(LEASE_SECONDS).bind(id).execute(&mut *transaction).await?;
        claims.push(TaskClaim {
            id,
            owner,
            generation: generation + 1,
            attempts,
        });
    }
    transaction.commit().await?;
    Ok(claims)
}

/// 副作用开始前与提交前都要检查；失去租约的旧调用只能丢弃本地结果。
pub(crate) async fn is_owned(context: &FeishuContext, claim: &TaskClaim) -> anyhow::Result<bool> {
    // tenant-boundary: raw-sql feishu-approval-execution-4
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feishu_approval_task WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review'")
        .bind(claim.id).bind(&claim.owner).bind(claim.generation)
        .fetch_one(context.approval_tasks().pool()).await?;
    Ok(count == 1)
}

/// 任务结论只由本代持有者提交；受影响行数是执行权是否仍成立的事实。
pub(crate) async fn finish(
    context: &FeishuContext,
    claim: &TaskClaim,
    row: &Record,
    retry: bool,
) -> anyhow::Result<bool> {
    let state: String = row.require("state")?;
    let reason: Option<String> = row.optional("last_error")?;
    let instance: Option<String> = row.optional("instance_code")?;
    let serial: Option<String> = row.optional("serial_number")?;
    let attempts = if retry {
        claim.attempts + 1
    } else {
        claim.attempts
    };
    let state = if retry && attempts >= 10 {
        "review"
    } else {
        &state
    };
    let delay = if retry {
        (1_i64 << attempts.min(8)).min(300)
    } else {
        30
    };
    // tenant-boundary: raw-sql feishu-approval-execution-5
    let result = sqlx::query("UPDATE feishu_approval_task SET state=?,last_error=?,instance_code=COALESCE(?,instance_code),serial_number=COALESCE(?,serial_number),attempts=?,available_at=UNIX_TIMESTAMP()+?,worker_id=NULL,lease_until=NULL,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND effect_phase IN ('ready','created')")
        .bind(state).bind(reason).bind(instance).bind(serial).bind(attempts).bind(delay)
        .bind(claim.id).bind(&claim.owner).bind(claim.generation)
        .execute(context.approval_tasks().pool()).await?;
    Ok(result.rows_affected() == 1)
}

/// 组 leader 所属的配置 id（执行权校验一体）。
///
/// # 为什么组函数都要先读它
///
/// 组键只在**同一配置内**唯一；`begin_group_effect` / `confirm_group_effect` /
/// `finish_group` 只拿到 `group_key`，写组行必须带上 `config_id` 才能精确定位。
/// 顺带在这一步校验执行权（worker/generation/租约/状态/组 leader 身份），
/// 后续 UPDATE 再用同一组条件兜底——两道之间租约可能刚好过期，第二道的
/// 影响行数是最终事实。
async fn leader_config_id_in_tx(
    connection: &mut sqlx::MySqlConnection,
    claim: &TaskClaim,
    group_key: &str,
) -> anyhow::Result<Option<i64>> {
    // tenant-boundary: raw-sql feishu-approval-group-leader-config
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT config_id FROM feishu_approval_task WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND group_id=? AND group_seq=0",
    )
    .bind(claim.id)
    .bind(&claim.owner)
    .bind(claim.generation)
    .bind(group_key)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(row)
}

/// 组副作用开始：**组行阶段与 leader 阶段同一事务**推进，并顺延 leader 租约。
///
/// 组行阶段是外部副作用的唯一事实（`reopen` 不会重置它），leader 的阶段只是认领
/// 依据的镜像；两者必须一起提交，否则崩溃后会出现「组行说创建过、leader 说没试过」
/// 或反之（设计 §4.1、§6.3）。
pub(crate) async fn begin_group_effect(
    context: &FeishuContext,
    claim: &TaskClaim,
    group_key: &str,
    phase: &str,
) -> anyhow::Result<()> {
    let mut transaction = context.approval_tasks().pool().begin().await?;
    let config_id = leader_config_id_in_tx(&mut transaction, claim, group_key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("聚合组 leader 执行权已失效"))?;
    // tenant-boundary: raw-sql feishu-approval-group-begin-leader
    let result = sqlx::query("UPDATE feishu_approval_task SET effect_phase=?,lease_until=UNIX_TIMESTAMP()+?,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND group_id=? AND group_seq=0 AND effect_phase IN ('ready','created')")
        .bind(phase).bind(LEASE_SECONDS).bind(claim.id).bind(&claim.owner).bind(claim.generation).bind(group_key)
        .execute(&mut *transaction).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "外部写入未开始：聚合组执行权失效或上次写入结果未知"
    );
    // tenant-boundary: raw-sql feishu-approval-group-begin-row
    let updated = sqlx::query(
        "UPDATE feishu_approval_group SET effect_phase=?,updated_at=UNIX_TIMESTAMP() WHERE config_id=? AND group_key=?",
    )
    .bind(phase)
    .bind(config_id)
    .bind(group_key)
    .execute(&mut *transaction)
    .await?;
    anyhow::ensure!(
        updated.rows_affected() == 1,
        "聚合组行不存在，外部写入未开始"
    );
    transaction.commit().await?;
    Ok(())
}

/// 组副作用确认：**实例码只写组行**，leader 只写阶段。
///
/// 这条分配是设计 §4.1 的核心不变量：leader 任务行会随单条点击摘组保留，若把组
/// 实例码写进去，摘出的行就带着别人的实例、事件也会被它截获。组实例码只属于组行。
pub(crate) async fn confirm_group_effect(
    context: &FeishuContext,
    claim: &TaskClaim,
    group_key: &str,
    phase: &str,
    instance: Option<&str>,
) -> anyhow::Result<()> {
    let mut transaction = context.approval_tasks().pool().begin().await?;
    let config_id = leader_config_id_in_tx(&mut transaction, claim, group_key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("聚合组 leader 执行权已失效"))?;
    // tenant-boundary: raw-sql feishu-approval-group-confirm-leader
    let result = sqlx::query("UPDATE feishu_approval_task SET effect_phase=?,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND group_id=? AND group_seq=0")
        .bind(phase).bind(claim.id).bind(&claim.owner).bind(claim.generation).bind(group_key)
        .execute(&mut *transaction).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "远端结果未能持久确认，禁止重放写入"
    );
    // tenant-boundary: raw-sql feishu-approval-group-confirm-row
    let updated = sqlx::query(
        "UPDATE feishu_approval_group SET effect_phase=?,instance_code=COALESCE(?,instance_code),updated_at=UNIX_TIMESTAMP() WHERE config_id=? AND group_key=?",
    )
    .bind(phase)
    .bind(instance)
    .bind(config_id)
    .bind(group_key)
    .execute(&mut *transaction)
    .await?;
    anyhow::ensure!(updated.rows_affected() == 1, "聚合组行在确认时不存在");
    transaction.commit().await?;
    Ok(())
}

/// 组收尾：**一个事务**内更新 leader（状态/退避/清租约）、全体成员状态、组行编号与阶段。
///
/// # 为什么不能复用单条 `finish`
///
/// 单条 `finish` 的阶段白名单是 `ready/created`，而组在 `60012` 之后会停在 `lookup`、
/// 创建前停在 `creating`——照抄会让组在「回捞成功」或「创建中途暂停」时收不了尾。
/// 组路径也不经过通用的 `record_outcome`，所以成员状态必须由这里一起写。
///
/// # 组行阶段
///
/// 编号到手 = 组实例已确认创建：组行 `effect_phase` 落实为 `created` 并记下编号。
/// 暂停/重试不动组行阶段——它是外部副作用的唯一事实，只有确认函数有权推进。
pub(crate) async fn finish_group(
    context: &FeishuContext,
    claim: &TaskClaim,
    group_key: &str,
    state: &str,
    serial: Option<&str>,
    reason: Option<&str>,
    retry: bool,
) -> anyhow::Result<bool> {
    let attempts = if retry {
        claim.attempts + 1
    } else {
        claim.attempts
    };
    let state = if retry && attempts >= 10 {
        "review"
    } else {
        state
    };
    let delay = if retry {
        (1_i64 << attempts.min(8)).min(300)
    } else {
        30
    };
    let mut transaction = context.approval_tasks().pool().begin().await?;
    let Some(config_id) = leader_config_id_in_tx(&mut transaction, claim, group_key).await? else {
        // 只读事务：显式回滚，别让 Drop 打印「事务被丢弃」。
        transaction.rollback().await?;
        return Ok(false);
    };
    // tenant-boundary: raw-sql feishu-approval-group-finish-leader
    let result = sqlx::query("UPDATE feishu_approval_task SET state=?,last_error=?,attempts=?,available_at=UNIX_TIMESTAMP()+?,worker_id=NULL,lease_until=NULL,updated_at=UNIX_TIMESTAMP() WHERE id=? AND worker_id=? AND generation=? AND lease_until>UNIX_TIMESTAMP() AND state<>'review' AND group_id=? AND group_seq=0 AND effect_phase IN ('ready','creating','lookup','created')")
        .bind(state).bind(reason).bind(attempts).bind(delay)
        .bind(claim.id).bind(&claim.owner).bind(claim.generation).bind(group_key)
        .execute(&mut *transaction).await?;
    if result.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(false);
    }
    // 成员行只承载自身状态：组实例身份一律不落到这里（设计 §4.1）。
    // tenant-boundary: raw-sql feishu-approval-group-finish-members
    sqlx::query(
        "UPDATE feishu_approval_task SET state=?,last_error=?,updated_at=UNIX_TIMESTAMP() WHERE config_id=? AND group_id=? AND group_seq>0",
    )
    .bind(state)
    .bind(reason)
    .bind(config_id)
    .bind(group_key)
    .execute(&mut *transaction)
    .await?;
    if let Some(serial) = serial.filter(|value| !value.trim().is_empty()) {
        // 编号到手 = 组实例已确认创建：组行阶段与编号一起落实。
        // tenant-boundary: raw-sql feishu-approval-group-finish-row
        let updated = sqlx::query(
            "UPDATE feishu_approval_group SET serial_number=?,effect_phase='created',updated_at=UNIX_TIMESTAMP() WHERE config_id=? AND group_key=?",
        )
        .bind(serial)
        .bind(config_id)
        .bind(group_key)
        .execute(&mut *transaction)
        .await?;
        anyhow::ensure!(updated.rows_affected() == 1, "聚合组行在收尾时不存在");
    }
    transaction.commit().await?;
    Ok(true)
}

/// 组内任务被单条点击时的摘组结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetachOutcome {
    /// 任务不属于任何组（含任务行已被删除的竞态）：调用方按普通单条语义继续。
    NotGrouped,
    /// 已摘出；`promoted` 是接手 leader 的成员任务 id（被摘的是 leader 且组内仍有成员）。
    Detached { promoted: Option<i64> },
    /// 已摘出且组内再无成员，组行已删除。
    GroupRemoved,
    /// 组内仍有未过期租约（正在执行），本轮不能摘。
    Busy,
}

/// 把一行组内任务摘出为单条任务（设计 §4.9）。
///
/// # 加锁顺序
///
/// 先锁配置行、再锁组内任务行——与播种的 `seed_batch_click` 同一顺序（配置 → 任务）。
/// 反向加锁会与「播种锁配置行后等任务行」形成环。
///
/// # 判定与写入同事务
///
/// 「组内有无有效租约」「谁是 leader」「摘完后还剩几行」全部取自行锁内复读的快照
/// （`lock_group_members_in_tx` 带服务端时间 `now`）；到提交前认领方改不动这些行，
/// 不会出现「判定无租约 → 摘组 → 实际删掉在途任务」的窗口（设计 §6.1）。
///
/// 摘出 leader 时把剩余组内序号最小的成员提升为 leader（`group_seq=0`）；组里没有
/// 剩余成员时删除组行。组行已有实例码时同样摘出：组实例继续由新 leader 收尾。
pub(crate) async fn detach_from_group(
    context: &FeishuContext,
    task_id: i64,
) -> anyhow::Result<DetachOutcome> {
    let located: Option<(i64, Option<String>)> =
        // tenant-boundary: raw-sql feishu-approval-group-detach-locate
        sqlx::query_as("SELECT config_id,group_id FROM feishu_approval_task WHERE id=?")
            .bind(task_id)
            .fetch_optional(context.approval_tasks().pool())
            .await?;
    let Some((config_id, Some(group_key))) = located else {
        return Ok(DetachOutcome::NotGrouped);
    };
    let mut transaction = context.approval_tasks().pool().begin().await?;
    // tenant-boundary: raw-sql feishu-approval-group-detach-lock-config
    sqlx::query_scalar::<_, i64>("SELECT id FROM feishu_approval_config WHERE id=? FOR UPDATE")
        .bind(config_id)
        .fetch_optional(&mut *transaction)
        .await?;
    // tenant-boundary: raw-sql feishu-approval-group-detach-now
    let now: i64 = sqlx::query_scalar("SELECT UNIX_TIMESTAMP()")
        .fetch_one(&mut *transaction)
        .await?;
    // tenant-boundary: raw-sql feishu-approval-group-detach-members
    let members: Vec<(i64, i64, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT id,group_seq,worker_id,lease_until FROM feishu_approval_task WHERE config_id=? AND group_id=? ORDER BY group_seq,id FOR UPDATE",
    )
    .bind(config_id)
    .bind(&group_key)
    .fetch_all(&mut *transaction)
    .await?;
    if members.is_empty() {
        transaction.rollback().await?;
        return Ok(DetachOutcome::NotGrouped);
    }
    if members
        .iter()
        .any(|(_, _, holder, lease)| live_lease(holder.as_deref(), *lease, now))
    {
        transaction.rollback().await?;
        return Ok(DetachOutcome::Busy);
    }
    let Some(target_seq) = members
        .iter()
        .find(|(id, _, _, _)| *id == task_id)
        .map(|(_, seq, _, _)| *seq)
    else {
        transaction.rollback().await?;
        return Ok(DetachOutcome::NotGrouped);
    };
    // tenant-boundary: raw-sql feishu-approval-group-detach-row
    let detached = sqlx::query(
        "UPDATE feishu_approval_task SET group_id=NULL,group_seq=0,updated_at=UNIX_TIMESTAMP() WHERE id=?",
    )
    .bind(task_id)
    .execute(&mut *transaction)
    .await?;
    anyhow::ensure!(detached.rows_affected() == 1, "任务行 {task_id} 摘组失败");
    let mut promoted = None;
    if target_seq == 0 {
        let next = members
            .iter()
            .filter(|(id, _, _, _)| *id != task_id)
            .min_by_key(|(_, seq, _, _)| *seq)
            .map(|(id, _, _, _)| *id);
        if let Some(next_id) = next {
            // tenant-boundary: raw-sql feishu-approval-group-detach-promote
            let updated = sqlx::query(
                "UPDATE feishu_approval_task SET group_seq=0,updated_at=UNIX_TIMESTAMP() WHERE id=?",
            )
            .bind(next_id)
            .execute(&mut *transaction)
            .await?;
            anyhow::ensure!(updated.rows_affected() == 1, "聚合组 leader 提升失败");
            promoted = Some(next_id);
        }
    }
    let group_removed = members.len() == 1;
    if group_removed {
        // tenant-boundary: raw-sql feishu-approval-group-detach-delete-row
        sqlx::query("DELETE FROM feishu_approval_group WHERE config_id=? AND group_key=?")
            .bind(config_id)
            .bind(&group_key)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    if group_removed {
        Ok(DetachOutcome::GroupRemoved)
    } else {
        Ok(DetachOutcome::Detached { promoted })
    }
}

/// 租约判定（与 `approval_aggregation::repository::has_live_lease` 同口径的裸列版本）。
///
/// 有持有者但没有到期时刻按「仍持有」处理：宁可本轮不动，等下一轮再判。
fn live_lease(holder: Option<&str>, lease_until: Option<i64>, now: i64) -> bool {
    let holder = holder.map(str::trim).filter(|value| !value.is_empty());
    match (holder, lease_until) {
        (Some(_), None) => true,
        (Some(_), Some(until)) => until > now,
        _ => false,
    }
}
