//! 出站拉取的表级编排：选表 → 解析列名 → 一次取回 → 逐字段派生 → 事务内落库与补集停用。
//!
//! **拉取合并、落库拆开**（设计 §6.2）：同一张表 N 个字段共用**一份**快照，
//! 落库却逐字段各一个事务。这既是「一轮只扫一次表」的收益来源，也让「一个字段写坏」
//! 不会把整轮已提交的结果拖回去。
//!
//! 本模块**不依赖 `ActionContext`**（后台 worker 没有可用 ctx，理由见
//! [`super::option_write`]），因此读走 `Repository`、事务走 `Database::transaction()`。
//! 一轮的循环本身在 worker（`infrastructure/feishu_pull.rs`）里——那里才拿得到
//! 告警发送器。
//!
//! # 一轮的成败判据
//!
//! **成功不是 `fetched != 0`**，而是「分页收敛」——`page_token` 耗尽且累计行数与
//! 首屏 `total` 相符（由 [`super::bitable::list_all_records`] 保证）。读成功且确实为空的
//! 表其实应当照常停用补集，否则选项集永久冻结、控制台却显示「刚刚同步」。
//!
//! 但「读成功且为空」有一个官方明示的歧义：多维表格开启高级权限而调用身份不在
//! 授权群内时，**可能出现调用成功但返回数据为空**。此时收敛断言会通过，若照常执行
//! 补集停用，会把该数据源 100% 已启用的选项静默停掉。所以顺序是：
//! 拿到的行数为 0 且库里仍有已启用行 → 判为**可疑空快照**，只记录、不停用。

use std::collections::{HashMap, HashSet};

use yang_base::table::Record;
use yang_base::BaseError;
use yang_db::Database;

use super::bitable::{
    cell_label, list_all_records, resolve_current_field_names, BitableCoordinates, CellValue,
    RecordsSnapshot,
};
use super::context::FeishuContext;
use super::derive::{derive_options, snapshot_digest, DerivedOption, RawValue};
use super::linkage::Linkage;
use super::option_write::{
    apply_option_rows, count_option_rows, disable_option_rows, find_foreign_option_owner,
    OptionWriteItem, OptionWriteOutcome,
};
use super::outbound::{OutboundFailure, OutboundTransport, Sleeper};
use super::tenant_token::TenantTokenProvider;
use crate::infrastructure::audit;

/// 拉取所需的共享依赖。借用而不是持有：worker 每轮现取，避免把资源生命周期拉长。
pub(crate) struct PullDeps<'a> {
    pub(crate) context: &'a FeishuContext,
    pub(crate) database: &'a Database,
    pub(crate) transport: &'a dyn OutboundTransport,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) tokens: &'a TenantTokenProvider,
    /// 单轮最多翻页数（透传给分页状态机）。
    pub(crate) max_pages: u32,
}

/// 一条**已解析过名字**的绑定：`field_id` 与它的当前 `field_name` 都在手边。
///
/// 表级拉取把「解析名字」与「拼请求参数」分成两步：先按 `field_id` 解析出当前名字
/// （决策 D2，改名不断链），再按名字拼 `field_names`。这个结构是两步之间的载体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundField {
    pub(crate) field_id: String,
    /// 本轮解析出来的**当前**名字（不是绑定行上的缓存）。
    pub(crate) field_name: String,
    /// 同表内的父列 `field_id`；无父为 `None`。**已归一化**：空白串按无父处理。
    pub(crate) parent_field_id: Option<String>,
}

/// `field_names` 要传的列名集合：**所有绑定列 + 所有父列**的并集，去重、保序。
///
/// 为什么父列也要限定：父键由**同行共现**读出（`derive.rs` 的 A7b）——不快照父列就
/// 拼不出父键，整棵子树会静默变成无父。
///
/// 为什么必须去重：`field_names` 是**一个** JSON 数组查询参数，重复项会被官方拒
/// （`bitable.rs:879` 的 `duplicate_field_names_are_rejected` 钉着这条）。三级链的
/// 中间列（既自己取值、又是别人的父）是最容易漏的那一例。
///
/// 只在**绑定集合内**找父：不在集合里的 `parent_field_id` 查不到名字，这里略过——
/// 「父列不在集合内」由 [`bind_fields`] 报错，不在这里静默降级。
pub(crate) fn collect_field_names(fields: &[BoundField]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut names: Vec<String> = Vec::new();
    for field in fields {
        let parent_name = field
            .parent_field_id
            .as_deref()
            .and_then(|parent| resolved_name(fields, parent));
        for name in std::iter::once(field.field_name.as_str()).chain(parent_name) {
            if seen.insert(name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// 在解析结果里按 `field_id` 取当前名字。
fn resolved_name<'a>(fields: &'a [BoundField], field_id: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|field| field.field_id == field_id)
        .map(|field| field.field_name.as_str())
}

/// 一条字段绑定（表级拉取视角）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableBinding {
    /// 绑定行的主键。落库按它定位（`source_key` 虽也唯一，但主键更精确）。
    pub(crate) id: i64,
    /// 进 URL 路径段的数据源标识；出站的落库边界就是它。
    pub(crate) source_key: String,
    /// 多维表格字段 ID——**身份**。
    pub(crate) field_id: String,
    /// 上一轮解析出的名字（缓存）。本轮以重新解析的结果为准。
    pub(crate) field_name: Option<String>,
    /// 同表内的父列 `field_id`；无父为 `None`。
    pub(crate) parent_field_id: Option<String>,
    /// 上一轮落库的内容摘要。归属是**这一条绑定**（设计 §6.3）。
    pub(crate) snapshot_digest: Option<String>,
}

/// 一张待拉取的表级数据源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullTable {
    /// 表级行主键。同步状态与审计都按它定位。
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) coordinates: BitableCoordinates,
    /// **启用中**的绑定（`enabled = true`）。空数组合法：一张表可以一个字段都没勾。
    pub(crate) bindings: Vec<TableBinding>,
}

/// 一张表一轮拉取的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TablePullOutcome {
    /// 拉到的记录行数。**一份快照服务整表**，所以这是表级数字。
    pub(crate) fetched: usize,
    /// 本轮处理的字段绑定条数。
    pub(crate) fields: usize,
    /// 内容未变而跳过写库的字段数。
    pub(crate) skipped: usize,
    pub(crate) inserted: u64,
    pub(crate) updated: u64,
    pub(crate) disabled: u64,
}

/// 选出本轮要拉的表级数据源，并把它们的启用绑定一次取回。
///
/// 判据是 `ingest_mode == pull` + `status == active` + 坐标齐备；配置单位是表，
/// 所以同表 N 个字段现在是**一行**。判据与 [`check_pullable`]（控制台点「立即拉取」
/// 时走的那道预检）必须逐条一致。
///
/// 绑定**一次 `where_in` 取回再分组**，不是按表查 N 次。`where_in` 拒绝空列表，
/// 所以候选表为空时要短路。
pub(crate) async fn load_pull_tables(context: &FeishuContext) -> Result<Vec<PullTable>, BaseError> {
    let rows = context
        .datasources()
        .query()
        .select_fields(&[
            "id",
            "title",
            "bitable_base_token",
            "bitable_table_id",
            "bitable_view_id",
        ])?
        .where_eq("ingest_mode", serde_json::json!("pull"))?
        .where_eq("status", serde_json::json!("active"))?
        .all()
        .await?;

    let mut tables = Vec::new();
    for row in rows {
        let title: String = row.optional::<String>("title")?.unwrap_or_default();
        // 缺坐标就不是一个可拉取的表，静默跳过会让「配了却不生效」无从排查，故记 warning。
        let Some(base_token) = trimmed(row.optional::<String>("bitable_base_token")?) else {
            tracing::warn!(title = %title, "表级数据源缺少 bitable_base_token，本轮跳过");
            continue;
        };
        let Some(table_id) = trimmed(row.optional::<String>("bitable_table_id")?) else {
            tracing::warn!(title = %title, "表级数据源缺少 bitable_table_id，本轮跳过");
            continue;
        };
        tables.push(PullTable {
            id: row.require("id")?,
            title,
            coordinates: BitableCoordinates {
                app_token: base_token,
                table_id,
                view_id: trimmed(row.optional::<String>("bitable_view_id")?),
            },
            bindings: Vec::new(),
        });
    }

    if tables.is_empty() {
        return Ok(tables);
    }

    let ids: Vec<serde_json::Value> = tables
        .iter()
        .map(|table| serde_json::json!(table.id))
        .collect();
    let binding_rows = context
        .datasource_fields()
        .query()
        .select_fields(&[
            "id",
            "datasource_id",
            "source_key",
            "field_id",
            "field_name",
            "parent_field_id",
            "snapshot_digest",
        ])?
        .where_in("datasource_id", ids)?
        // 只取启用中的：取消勾选 = 停用绑定（T6 的决定），停用的列不参与本轮拉取。
        .where_eq("enabled", serde_json::json!(true))?
        .all()
        .await?;

    let mut groups: HashMap<i64, Vec<TableBinding>> = HashMap::new();
    for row in binding_rows {
        groups
            .entry(row.require("datasource_id")?)
            .or_default()
            .push(TableBinding {
                id: row.require("id")?,
                source_key: row.require("source_key")?,
                field_id: row.require("field_id")?,
                field_name: row.optional("field_name")?,
                parent_field_id: row.optional("parent_field_id")?,
                snapshot_digest: row.optional("snapshot_digest")?,
            });
    }
    for table in &mut tables {
        // 没配绑定的表得到**空数组**，不是缺键。
        table.bindings = groups.remove(&table.id).unwrap_or_default();
    }
    Ok(tables)
}

// ---------------------------------------------------------------------------
// 表级拉取（设计 §6.2）
// ---------------------------------------------------------------------------

/// 一张表**现在为什么拉不动**。
///
/// 每个变体对应一个不同的修法，所以刻意不合并成一个笼统的「不可拉取」——
/// 运维拿到这句话是要回去改配置的，笼统等于没答。
///
/// **没有「缺取数列字段名」这一项**：取数列已经上移到字段绑定层，表级行上只有一个
/// 坐标三元组。绑定一条都没勾的表不是「配置不全」，它只是没有要与飞书同步的东西
/// （`pull_table` 会直接返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotPullable {
    /// 取数方式是 `push`：服务端不主动出网，等飞书多维表格自动化来推。
    PushMode,
    /// 已停用。
    Disabled,
    /// 缺 Base Token。
    MissingBaseToken,
    /// 缺数据表 ID。
    MissingTableId,
}

impl NotPullable {
    /// 给人看的一句话，必须点出**具体**缺什么。
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::PushMode => "取数方式是「手工推送」——服务端不主动出网，只有「定时拉取」才拉得动",
            Self::Disabled => "数据源已停用",
            Self::MissingBaseToken => "缺少 Base Token",
            Self::MissingTableId => "缺少数据表 ID",
        }
    }
}

/// 判定一张表**现在**能不能被拉取（自动轮询不经过这里，它只跑候选集）。
///
/// 判据必须与 [`load_pull_tables`] 的 WHERE 子句逐条对应（`ingest_mode == pull` +
/// `status == active`），再加上它随后 trim 判空的两个坐标。两边漂移的后果**不对称**：
/// 这里宽了，用户点完按钮只能等到前端轮询超时；这里严了，一张其实拉得动的表被白拒。
///
/// 空串与 `NULL` 在库里是两种形态，但对「能不能拉」是同一件事——`load_pull_tables`
/// 也是 trim 之后判空的。
pub(crate) fn check_pullable(
    ingest_mode: &str,
    status: &str,
    base_token: Option<&str>,
    table_id: Option<&str>,
) -> Result<(), NotPullable> {
    if ingest_mode != "pull" {
        return Err(NotPullable::PushMode);
    }
    if status != "active" {
        return Err(NotPullable::Disabled);
    }
    for (value, missing) in [
        (base_token, NotPullable::MissingBaseToken),
        (table_id, NotPullable::MissingTableId),
    ] {
        if !value.is_some_and(|text| !text.trim().is_empty()) {
            return Err(missing);
        }
    }
    Ok(())
}

/// 一张表的一轮拉取：**一次取回、按字段分派**。
///
/// # 为什么这样切
///
/// 逐字段各扫一遍表是 **N 次全表扫描**；这里把「取回」合并成一次、把「落库」拆成
/// 每字段一个事务（§6.2 末句「拉取合并、落库拆开」）。
///
/// # 状态列的归属（设计 §6.3）
///
/// `snapshot_digest` 落在**字段绑定**行——一个字段内容没变就该跳过它的写库；
/// `last_pull_at` / `last_success_at` / `consecutive_failures` / `last_error`
/// 落在**表级**行——失败是整轮的（决策 D4）。
///
/// 本函数只写**成功**那一组状态。失败状态由调用方在失败路径上写：表级失败语义与
/// 告警（自增 `consecutive_failures`、写 `last_error`、达阈值发邮件）是 T10 的内容，
/// 两边都写会让连续失败次数翻倍。
///
/// 一张表启用中的绑定为空时直接返回：它本来就没有要与飞书同步的东西。
pub(crate) async fn pull_table(
    deps: &PullDeps<'_>,
    table: &PullTable,
) -> Result<TablePullOutcome, BaseError> {
    let outcome = pull_table_inner(deps, table).await?;
    record_table_success(deps, table.id).await?;
    Ok(outcome)
}

/// 表级编排的本体。拆出来只是为了让 [`pull_table`] 的成功状态收尾一眼可见。
async fn pull_table_inner(
    deps: &PullDeps<'_>,
    table: &PullTable,
) -> Result<TablePullOutcome, BaseError> {
    let mut outcome = TablePullOutcome::default();
    if table.bindings.is_empty() {
        return Ok(outcome);
    }

    // 1) 一次元数据往返，解析整表勾选列（含父列）的**当前**名字。
    //    缺失即整体失败（全有或全无）：`resolve_field_names` 会点名每一个找不到的
    //    `field_id`，运维一次就能看全要修的东西。
    let resolved = resolve_current_field_names(
        deps.transport,
        deps.sleeper,
        deps.tokens,
        &table.coordinates,
        &resolve_targets(&table.bindings),
    )
    .await
    .map_err(internal_error)?;
    let bound = bind_fields(&table.bindings, &resolved)?;

    // 2) 一次取回一份快照，服务整表所有字段——这是表级拉取的全部意义。
    let snapshot = list_all_records(
        deps.transport,
        deps.sleeper,
        deps.tokens,
        &table.coordinates,
        &collect_field_names(&bound),
        deps.max_pages.max(1),
    )
    .await
    .map_err(internal_error)?;
    outcome.fetched = snapshot.items.len();

    // 3) 空快照歧义守卫。**同一份快照服务整表**，所以这判据也是表级的：只要还有任一
    //    字段本地留着已启用选项，整张表本轮一个写都不做（理由见模块文档）。
    if snapshot.items.is_empty() {
        let mut suspicious = Vec::new();
        for binding in &table.bindings {
            let (_, active) = count_existing(deps, &binding.source_key).await?;
            if active > 0 {
                suspicious.push(binding.source_key.clone());
            }
        }
        if !suspicious.is_empty() {
            tracing::warn!(
                table_id = table.id,
                suspicious = %suspicious.join("、"),
                "飞书返回空快照但本地仍有已启用选项：疑似文档权限不足（官方明示高级权限下\
                 可能「调用成功但返回空」），本轮拒绝停用补集"
            );
            return Err(BaseError::ConfigError(format!(
                "飞书返回空快照但本地仍有已启用选项（{}）：疑似文档权限不足（官方明示\
                 高级权限下可能「调用成功但返回空」），本轮拒绝停用补集",
                suspicious.join("、")
            )));
        }
    }

    // 4) 逐条绑定：抽取 → 派生 → 比对 → 落库（每条一个事务）。
    for (binding, field) in table.bindings.iter().zip(bound.iter()) {
        outcome.fields += 1;
        let linkage = parent_linkage(field, &table.bindings, &bound)?;
        let values = extract_values_owned(&snapshot, &field.field_name, linkage.as_ref());
        // 派生要 `&[RawValue]`；这里把自有载体借出去，派生完即丢。
        let raw: Vec<RawValue<'_>> = values.iter().map(OwnedRawValue::as_raw).collect();
        let derived = derive_options(
            &binding.source_key,
            linkage
                .as_ref()
                .map(|linkage| linkage.parent_source_key.as_str()),
            &raw,
        );
        let digest = snapshot_digest(&derived);
        let renamed = name_cache_is_stale(binding, &field.field_name);

        let (existing_rows, active_rows) = count_existing(deps, &binding.source_key).await?;
        let disabled_rows = existing_rows.saturating_sub(active_rows);

        // 内容未变**且**本地没有已停用行 → 没有选项要写。
        //
        // 「没有已停用行」这个附加条件不可省：摘要是按**本轮应当是什么**算的，不含
        // `enabled`；只比摘要会让「被补集停用后内容恰好没变」的行永远复活不了。
        let unchanged =
            binding.snapshot_digest.as_deref() == Some(digest.as_str()) && disabled_rows == 0;
        if unchanged {
            outcome.skipped += 1;
        }

        // 跨源夺取预检与补集扫描都在进事务前做完：前者是可归因的业务失败，
        // 后者只读。两者都按 `source_key` 界定。
        let mut doomed = Vec::new();
        if !unchanged {
            let ids: Vec<String> = derived
                .iter()
                .map(|option| option.option_id.clone())
                .collect();
            // 与入站写入同一道预检：不让一个数据源改写另一个数据源的选项归属。
            if let Some((option_id, owner)) =
                find_foreign_option_owner(deps.context.options(), &binding.source_key, &ids).await?
            {
                return Err(BaseError::ConfigError(format!(
                    "选项 id {option_id} 已属于数据源 {owner}，拒绝改写其归属"
                )));
            }
            doomed = find_doomed(deps, &binding.source_key, &derived).await?;
        }

        // 内容与名字都没变时，这一行真的不用碰。
        if unchanged && !renamed {
            tracing::debug!(source_key = %binding.source_key, "内容未变，跳过写库");
            continue;
        }

        let written = persist_binding(
            deps,
            table.id,
            binding,
            BindingWritePlan {
                field_name: &field.field_name,
                digest: &digest,
                derived: &derived,
                doomed: &doomed,
                replace_options: !unchanged,
            },
        )
        .await?;
        outcome.inserted += written.options.inserted;
        outcome.updated += written.options.updated;
        outcome.disabled += written.disabled;

        tracing::info!(
            source_key = %binding.source_key,
            fetched = outcome.fetched,
            derived = derived.len(),
            inserted = written.options.inserted,
            updated = written.options.updated,
            disabled = written.disabled,
            "飞书数据源字段同步完成"
        );
    }
    Ok(outcome)
}

/// 一条绑定本轮落库的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BindingWrite {
    options: OptionWriteOutcome,
    disabled: u64,
}

/// 一条绑定本轮要落库的东西。
///
/// 收成一个结构体而不是一路传参：这些值同进同出，而且它们的**关系**（摘要配这批派生
/// 结果、补集配这个 `source_key`）比各自的类型更值得在签名里说清楚。
struct BindingWritePlan<'a> {
    /// 本轮解析出来的当前字段名，写回缓存。
    field_name: &'a str,
    /// 本轮派生结果的内容摘要，写回绑定行。
    digest: &'a str,
    /// 本轮派生出的选项（整行替换的输入）。
    derived: &'a [DerivedOption],
    /// 补集：本地有、本轮派生结果里没有的 `option_id`。
    doomed: &'a [String],
    /// 是否重写选项行。`false` = 内容没变、只是名字过期。
    replace_options: bool,
}

/// 一条绑定的事务内落库：选项整行替换 + 补集停用 + 名字与摘要 + 审计。
///
/// 名字缓存与摘要**总是**写：名字每一轮都要跟上飞书侧（决策 D2，改名不断链），
/// 摘要写在这里让下一轮能跳过。`replace_options = false` 时（内容没变、只是名字过期）
/// 不动任何选项行，自然也不追加审计——「只在真变化时追加」的语义要保住。
async fn persist_binding(
    deps: &PullDeps<'_>,
    table_id: i64,
    binding: &TableBinding,
    plan: BindingWritePlan<'_>,
) -> Result<BindingWrite, BaseError> {
    let options = deps.context.options();

    let mut binding_update = Record::new();
    binding_update.insert("field_name", serde_json::json!(plan.field_name));
    // 摘要与选项行同事务提交：半提交会让「摘要已推进但行没写完」永久错位。
    binding_update.insert("snapshot_digest", serde_json::json!(plan.digest));

    let mut transaction = deps.database.transaction().await?;
    let result = async {
        let mut written = BindingWrite::default();
        if plan.replace_options {
            let items: Vec<OptionWriteItem> = plan
                .derived
                .iter()
                .map(|option| to_write_item(&binding.source_key, option))
                .collect();
            written.options =
                apply_option_rows(options, &mut transaction, &binding.source_key, &items).await?;
            written.disabled =
                disable_option_rows(options, &mut transaction, &binding.source_key, plan.doomed)
                    .await?;

            let event = audit::succeeded_system_event_without_ctx(
                "feishu-pull",
                "feishu.pull_options",
                Some(audit::entity("feishu_datasource", table_id)?),
                audit::entity("feishu_option", &binding.source_key)?,
                audit::summary([
                    ("outcome_code", serde_json::json!("pulled")),
                    ("option_count", serde_json::json!(plan.derived.len() as i64)),
                    ("disabled_count", serde_json::json!(written.disabled as i64)),
                ])?,
            )?;
            audit::append_in_tx(&mut transaction, &event).await?;
        }
        deps.context
            .datasource_fields()
            .query()
            .where_eq("id", serde_json::json!(binding.id))?
            .update_in_tx(&mut transaction, binding_update)
            .await?;
        Ok::<_, BaseError>(written)
    }
    .await;
    FeishuContext::finish_transaction(transaction, result).await
}

/// 记录一次整表成功：表级时间戳推进、连续失败清零。
///
/// 独立事务（不与某个绑定共用）：这两列描述的是**整轮**，而落库是逐字段的。
/// 一个字段写到一半失败时整轮在调用方落成失败，这一组状态就不会被写。
async fn record_table_success(deps: &PullDeps<'_>, table_id: i64) -> Result<(), BaseError> {
    let mut transaction = deps.database.transaction().await?;
    let mut update = Record::new();
    update.insert("last_pull_at", serde_json::json!(now_seconds()));
    update.insert("last_success_at", serde_json::json!(now_seconds()));
    update.insert("consecutive_failures", serde_json::json!(0));
    update.insert("last_error", serde_json::Value::Null);
    let result = deps
        .context
        .datasources()
        .query()
        .where_eq("id", serde_json::json!(table_id))?
        .update_in_tx(&mut transaction, update)
        .await;
    match result {
        Ok(_) => transaction.commit().await.map_err(BaseError::from),
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

/// 记录一次整表失败：`consecutive_failures` 自增、写 `last_error`，返回自增后的次数。
///
/// 与 [`record_table_success`] 成对：成功清零、失败自增，两个都落在**表级**行上
/// （设计 §6.3——失败是整轮的，决策 D4）。分工不重叠：本函数只写失败那一组，
/// 成功那一组由 [`pull_table`] 的收尾写，两边都写会让连续失败次数翻倍。
///
/// 返回自增后的计数，因为**告警阈值判的正是它**（T10）：调用方拿到它就不必再查一次库
/// ——两次读之间可能有另一轮插进来。
///
/// 表级行已不存在（本轮进行中被删）时返回 `0` 且不写库：没有行可记账，也没有人可告警。
pub(crate) async fn record_table_failure(
    deps: &PullDeps<'_>,
    table_id: i64,
    message: &str,
) -> Result<i64, BaseError> {
    let current = deps
        .context
        .datasources()
        .query()
        .select_fields(&["consecutive_failures"])?
        .where_eq("id", serde_json::json!(table_id))?
        .optional()
        .await?;
    let Some(current) = current else {
        tracing::warn!(table_id, "表级数据源已不存在，本轮失败状态无处记账");
        return Ok(0);
    };
    let failures: i64 = current.optional("consecutive_failures")?.unwrap_or(0);
    // 错误文案可能很长（含飞书原始响应），截断后再落库。
    let message: String = message.chars().take(1000).collect();

    let mut transaction = deps.database.transaction().await?;
    let mut update = Record::new();
    update.insert("consecutive_failures", serde_json::json!(failures + 1));
    // 失败也是一次**尝试**：`last_pull_at` 是「最近拉取时间」，试过就该推进它。
    // 不推进的话，一张一直拉不动的表在控制台上会显示成「从来没拉过」，而
    // 「刚试过、又失败了」才是运维要的那条信息。
    update.insert("last_pull_at", serde_json::json!(now_seconds()));
    update.insert("last_error", serde_json::json!(message));
    let result = deps
        .context
        .datasources()
        .query()
        .where_eq("id", serde_json::json!(table_id))?
        .update_in_tx(&mut transaction, update)
        .await;
    match result {
        Ok(_) => {
            transaction.commit().await.map_err(BaseError::from)?;
            Ok(failures + 1)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// trim 后非空才算有值——`NULL` 与空白串对「能不能拉」是同一件事。
fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 绑定声明的父列 `field_id`（空白串按无父处理）。
///
/// 界面清空父列时可能提交空串而不是 `null`（`validate_fields` 已按同一口径处理），
/// 拿一个空 id 去解析会让整表因为一个不存在的字段停摆。
fn declared_parent(binding: &TableBinding) -> Option<&str> {
    binding
        .parent_field_id
        .as_deref()
        .map(str::trim)
        .filter(|parent| !parent.is_empty())
}

/// 本轮要交给「列出字段」解析的 `field_id` 全集：绑定列 + 父列，去重保序。
///
/// 与 [`collect_field_names`] 是同一件事的两个阶段：这里给的是 **id**（解析的输入），
/// 那里给的是**名字**（查询参数的输入）。父列也要解析——快照里靠名字取父列的值。
fn resolve_targets(bindings: &[TableBinding]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut ids = Vec::new();
    for binding in bindings {
        for id in std::iter::once(binding.field_id.as_str()).chain(declared_parent(binding)) {
            if seen.insert(id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// 把「列出字段」的解析结果贴回绑定。
///
/// 返回的顺序与入参 `bindings` **逐一对应**——派生父键要靠这个对应关系取父的
/// `source_key`。
///
/// # 父列不在集合里为什么是失败而不是降级
///
/// 集合来自「启用中的绑定」。父列不在里面只有两种可能：启用位与父指针漂移
/// （`validate_fields` 不允许，但手工改库可以），或父列的绑定行被删了。两种情况都
/// 读不到父列的文案——父列进不了 `field_names`、快照里也没有它。此时若静默按「无父」
/// 派生，子选项的 `option_id` 会**整棵子树一起变**：飞书上已选中的值全部失联，
/// 而且**不报错**（失效形态是「下拉静默变空」）。所以整体失败并点名，符合 D4。
fn bind_fields(
    bindings: &[TableBinding],
    resolved: &[(String, String)],
) -> Result<Vec<BoundField>, BaseError> {
    let name_of = |field_id: &str| {
        resolved
            .iter()
            .find(|(id, _)| id == field_id)
            .map(|(_, name)| name.clone())
    };

    let mut bound = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let Some(field_name) = name_of(&binding.field_id) else {
            return Err(BaseError::ConfigError(format!(
                "多维表格里找不到字段绑定 {} 的 field_id {}",
                binding.source_key, binding.field_id
            )));
        };
        let parent_field_id = declared_parent(binding).map(str::to_string);
        if let Some(parent) = parent_field_id.as_deref() {
            if !bindings.iter().any(|other| other.field_id == parent) {
                return Err(BaseError::ConfigError(format!(
                    "字段绑定 {} 的父列 {parent} 不在本表的启用绑定里：读不到父列文案会让\
                     整棵子树的选项 id 变化（飞书控件上已选中的值会失联），本轮整表停止",
                    binding.source_key
                )));
            }
        }
        bound.push(BoundField {
            field_id: binding.field_id.clone(),
            field_name,
            parent_field_id,
        });
    }
    Ok(bound)
}

/// 一条绑定的父列声明：父的 `source_key` 与父的**当前**名字。
///
/// 名字取**解析后**的那一份，不能用绑定行上的缓存——缓存是上一轮的名字，父列改名后
/// 按旧名字读快照会一片空白（`record.fields.get` 取不到键，全部子项静默变成无父）。
fn parent_linkage(
    field: &BoundField,
    bindings: &[TableBinding],
    bound: &[BoundField],
) -> Result<Option<Linkage>, BaseError> {
    let Some(parent_field_id) = field.parent_field_id.as_deref() else {
        return Ok(None);
    };
    let (Some(parent_binding), Some(parent_field)) = (
        bindings
            .iter()
            .find(|other| other.field_id == parent_field_id),
        bound.iter().find(|other| other.field_id == parent_field_id),
    ) else {
        // `bind_fields` 已经挡过一次；这是同一条判据的第二道，防的是将来有人绕过它
        // 单独调用本函数。
        return Err(BaseError::ConfigError(format!(
            "字段绑定 {} 的父列 {parent_field_id} 不在本表的启用绑定里",
            field.field_id
        )));
    };
    Ok(Some(Linkage {
        parent_source_key: parent_binding.source_key.clone(),
        parent_field: parent_field.field_name.clone(),
    }))
}

/// 绑定行的 `field_name` 缓存是否落后于本轮解析出来的名字。
///
/// 缓存为空（首次拉取）也算落后：名字要写回去，否则控制台一直显示空。
/// 两侧都 trim 过（`resolve_field_names` 与写回时同一个值），所以口径一致。
fn name_cache_is_stale(binding: &TableBinding, resolved: &str) -> bool {
    binding.field_name.as_deref() != Some(resolved)
}

/// 出站失败 → 内部错误。
///
/// 保留「要不要重试」这层语义：`Fatal` 是**配置错误**（列被删了、没给应用加文档
/// 权限），退避重试不会自愈，落成 `ConfigError` 让人回去改配置；其余是上游暂时不可用。
/// 文案原样保留——它是运维唯一的线索。
fn internal_error(failure: OutboundFailure) -> BaseError {
    match failure.kind {
        super::outbound::FailureKind::Fatal { .. } => BaseError::ConfigError(failure.to_string()),
        _ => BaseError::UpstreamUnavailable(failure.to_string()),
    }
}

/// 抽取取值对。与 [`extract_values`] 的区别是**自有字符串**，避免把借用透传到
/// 派生之外（派生只在本函数返回后调用一次，没有必要为零拷贝引入生命周期参数）。
fn extract_values_owned(
    snapshot: &RecordsSnapshot,
    field_name: &str,
    linkage: Option<&Linkage>,
) -> Vec<OwnedRawValue> {
    let mut values = Vec::new();
    let mut unsupported = 0usize;
    for record in &snapshot.items {
        let Some(value) = record.fields.get(field_name) else {
            continue;
        };
        let label = match cell_label(value) {
            CellValue::Text(label) => label,
            CellValue::Empty => continue,
            CellValue::Unsupported => {
                unsupported += 1;
                continue;
            }
        };
        let parent_label = linkage.and_then(|linkage| {
            record
                .fields
                .get(&linkage.parent_field)
                .and_then(|value| match cell_label(value) {
                    CellValue::Text(label) => Some(label),
                    _ => None,
                })
        });
        values.push(OwnedRawValue {
            parent_label,
            label,
        });
    }
    if unsupported > 0 {
        // 非空告警：静默丢弃会让「取数列选错类型」表现为「选项莫名变少」。
        tracing::warn!(
            unsupported,
            field_name,
            "取数列存在取值不支持的单元格，已跳过（多值列或人员/地理位置等无文本类型）"
        );
    }
    values
}

/// [`RawValue`] 的自有版本。
#[derive(Debug, Clone)]
struct OwnedRawValue {
    parent_label: Option<String>,
    label: String,
}

impl OwnedRawValue {
    fn as_raw(&self) -> RawValue<'_> {
        RawValue {
            parent_label: self.parent_label.as_deref(),
            label: self.label.as_str(),
        }
    }
}

/// 把派生结果转成待写入的行（**整行替换**：把本轮派生的全部列一并写）。
///
/// `enabled` 必须显式写 `true`：出站拉取是整行替换，某行被补集停用后又重新出现时
/// 要能复活。这与入站 upsert 的「合并」语义不同，也是本函数不复用 `to_record` 的原因。
fn to_write_item(source_key: &str, option: &DerivedOption) -> OptionWriteItem {
    let mut record = Record::new();
    record.insert("option_id", serde_json::json!(option.option_id));
    record.insert("source_key", serde_json::json!(source_key));
    record.insert("label", serde_json::json!(option.label));
    record.insert("sort_order", serde_json::json!(option.sort_order));
    record.insert("enabled", serde_json::json!(true));
    record.insert("parent_key", serde_json::json!(option.parent_key));
    record.insert("last_push_at", serde_json::json!(now_seconds()));
    OptionWriteItem {
        option_id: option.option_id.clone(),
        record,
    }
}

/// 补集扫描的每页大小。**必须 ≤ 框架硬上限**：`TableQuery::page` 对超限是
/// **拒绝**而不是 clamp，越界会让整轮拉取在补集那一步必败。
const SCAN_PAGE_SIZE: usize = 100;

/// 编译期守护，同 `approval_options.rs` 里那条：`MAX_TABLE_QUERY_PAGE_SIZE` 一旦
/// 被调低，这里构建期就炸，而不是等到拉取时才发现整轮必败。
///
/// **这不是假想的风险**——本仓库已经因为同一类越界挂过一次端点（110 条单测全绿、
/// 端点全挂），而这次是拉取侧：`find_doomed` 曾经写 `.page(1, 20_000)`，
/// 每一轮拉取都在补集那一步失败，单元测试到不了（那条路径要数据库）。
const _: () = assert!(SCAN_PAGE_SIZE <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 找出「本地有、本轮派生结果里没有」的选项 id（补集）。
///
/// 规模保护分两步：**先 `count` 判是否超限，再分页扫**。顺序不能反——
/// 先扫再判的话，大表会把上限那么多行读进来才发现该跳过。
async fn find_doomed(
    deps: &PullDeps<'_>,
    source_key: &str,
    derived: &[DerivedOption],
) -> Result<Vec<String>, BaseError> {
    /// 单轮补集停用的规模上限：超过就**跳过本轮停用**，只告警。
    const MAX_COMPLEMENT: usize = 20_000;

    let live: HashSet<&str> = derived
        .iter()
        .map(|option| option.option_id.as_str())
        .collect();

    let enabled_query = || -> Result<yang_base::table::TableQuery, BaseError> {
        deps.context
            .options()
            .query()
            .select_fields(&["option_id"])?
            .where_eq("source_key", serde_json::json!(source_key))?
            .where_eq("enabled", serde_json::json!(true))
    };

    // 第一步：先问总数。超限就没有必要把行读进来。
    let enabled = enabled_query()?.count().await? as usize;
    if enabled >= MAX_COMPLEMENT {
        tracing::warn!(
            source_key,
            enabled,
            limit = MAX_COMPLEMENT,
            "已启用选项达到单轮上限，本轮跳过补集停用（未确保视图完整时不做批量停用）"
        );
        return Ok(Vec::new());
    }

    // 第二步：分页扫完。`count` 已保证行数在上限内，所以页数有界（≤ 200）。
    let mut rows = Vec::with_capacity(enabled);
    let mut page = 1usize;
    loop {
        let batch = enabled_query()?.page(page, SCAN_PAGE_SIZE)?.all().await?;
        let fetched = batch.len();
        rows.extend(batch);
        if fetched < SCAN_PAGE_SIZE {
            break;
        }
        page += 1;
    }

    let mut doomed = Vec::new();
    for row in &rows {
        let option_id: String = row.require("option_id")?;
        if !live.contains(option_id.as_str()) {
            doomed.push(option_id);
        }
    }
    Ok(doomed)
}

/// 统计该数据源的存量行数与已启用行数。
async fn count_existing(deps: &PullDeps<'_>, source_key: &str) -> Result<(u64, u64), BaseError> {
    let total = count_option_rows(deps.context.options(), source_key).await?;
    let active = deps
        .context
        .options()
        .query()
        .select_fields(&["option_id"])?
        .where_eq("source_key", serde_json::json!(source_key))?
        .where_eq("enabled", serde_json::json!(true))?
        .count()
        .await?;
    Ok((total, active))
}

/// 一张表级数据源是否仍然存在且为 active。
///
/// 每张表开跑前**重读**：管理员可能在本轮进行中把它删掉或停用，此时必须丢弃本轮
/// 对它的全部写入。
pub(crate) async fn table_is_still_active(
    deps: &PullDeps<'_>,
    table_id: i64,
) -> Result<bool, BaseError> {
    let row = deps
        .context
        .datasources()
        .query()
        .select_fields(&["status"])?
        .where_eq("id", serde_json::json!(table_id))?
        .optional()
        .await?;
    match row {
        Some(row) => Ok(row.optional::<String>("status")?.as_deref() == Some("active")),
        None => Ok(false),
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::bitable::{RecordItem, RecordsSnapshot};
    use serde_json::json;

    fn snapshot(rows: Vec<(&str, &str)>) -> RecordsSnapshot {
        let items = rows
            .into_iter()
            .map(|(child, parent)| {
                let mut fields = std::collections::BTreeMap::new();
                fields.insert("子".to_string(), json!(child));
                fields.insert("父".to_string(), json!(parent));
                RecordItem {
                    record_id: "rec".to_string(),
                    fields,
                }
            })
            .collect::<Vec<_>>();
        RecordsSnapshot {
            total: items.len() as i64,
            items,
        }
    }

    /// 级联声明。形状只有两个成员——`cascade_field` 已随「形状收敛到
    /// `domain/linkage.rs`」一并去掉（它零消费）。
    fn linkage() -> Linkage {
        Linkage {
            parent_source_key: "currency".to_string(),
            parent_field: "父".to_string(),
        }
    }

    #[test]
    fn values_pair_child_with_the_same_rows_parent() {
        // 父键由**同行共现**读出，这是 A7b 的依据
        let snap = snapshot(vec![("6.9025", "USD 美元"), ("0.8807", "HKD 港币")]);
        let values = extract_values_owned(&snap, "子", Some(&linkage()));
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].label, "6.9025");
        assert_eq!(values[0].parent_label.as_deref(), Some("USD 美元"));
        assert_eq!(values[1].parent_label.as_deref(), Some("HKD 港币"));
    }

    #[test]
    fn without_linkage_parents_are_absent() {
        let snap = snapshot(vec![("甲", "父值")]);
        let values = extract_values_owned(&snap, "子", None);
        assert_eq!(values.len(), 1);
        assert!(values[0].parent_label.is_none());
    }

    #[test]
    fn empty_cells_do_not_produce_values() {
        let snap = snapshot(vec![("", "USD"), ("6.9025", "USD")]);
        let values = extract_values_owned(&snap, "子", Some(&linkage()));
        assert_eq!(values.len(), 1, "空单元格不贡献任何值");
        assert_eq!(values[0].label, "6.9025");
    }

    #[test]
    fn missing_child_column_yields_nothing() {
        // 取数列名配错时不是报错，而是没有任何值——上层会看到 derived=0
        let snap = snapshot(vec![("甲", "父值")]);
        assert!(extract_values_owned(&snap, "不存在的列", None).is_empty());
    }

    #[test]
    fn unsupported_cell_shapes_are_skipped() {
        let mut fields = std::collections::BTreeMap::new();
        // 人员列：对象数组，不含 text 键
        fields.insert("子".to_string(), json!([{"id": "ou_x", "name": "张三"}]));
        let snap = RecordsSnapshot {
            total: 1,
            items: vec![RecordItem {
                record_id: "rec".to_string(),
                fields,
            }],
        };
        assert!(extract_values_owned(&snap, "子", None).is_empty());
    }

    #[test]
    fn write_item_replaces_the_whole_row_and_reenables() {
        // 整行替换：`enabled` 显式写 true，否则「停用→重新出现」的行永远复活不了
        let option = DerivedOption {
            option_id: "demo:abc".to_string(),
            label: "北京".to_string(),
            parent_key: String::new(),
            sort_order: 3,
        };
        let item = to_write_item("demo", &option);
        assert_eq!(item.option_id, "demo:abc");
        let record = item.record;
        assert_eq!(record.get("enabled"), Some(&json!(true)));
        assert_eq!(record.get("sort_order"), Some(&json!(3)));
        assert_eq!(record.get("source_key"), Some(&json!("demo")));
        assert_eq!(record.get("parent_key"), Some(&json!("")));
        assert!(
            record.get("last_push_at").is_some(),
            "整行替换必须带上同步信号，否则控制台看到的存活时间会停在旧值"
        );
        // i18n / is_default 不属出站路径拥有的列，不得被写（否则会把入站推来的值抹掉）
        assert!(record.get("i18n").is_none());
        assert!(record.get("is_default").is_none());
    }

    #[test]
    fn owned_raw_value_borrows_correctly() {
        let owned = OwnedRawValue {
            parent_label: Some("USD".to_string()),
            label: "6.9".to_string(),
        };
        let raw = owned.as_raw();
        assert_eq!(raw.label, "6.9");
        assert_eq!(raw.parent_label, Some("USD"));
    }

    /// 预检必须**拒绝**，并给出原因。断言的是拒绝本身——通过就意味着这条用例的前提塌了。
    fn refusal(
        ingest_mode: &str,
        status: &str,
        base_token: Option<&str>,
        table_id: Option<&str>,
    ) -> NotPullable {
        match check_pullable(ingest_mode, status, base_token, table_id) {
            Ok(()) => panic!("这张表不该通过预检：{ingest_mode}/{status}"),
            Err(reason) => reason,
        }
    }

    #[test]
    fn a_push_source_cannot_be_pulled_on_demand() {
        // 最要紧的一条：手工推送的源**根本不进** load_pull_tables 的候选集，
        // worker 会静默跳过。不在触发前挡掉，用户点完按钮只能看到轮询超时，
        // 而真实原因永远不会浮出来。
        assert_eq!(
            refusal("push", "active", Some("b"), Some("t")),
            NotPullable::PushMode
        );
    }

    #[test]
    fn a_disabled_source_cannot_be_pulled_on_demand() {
        assert_eq!(
            refusal("pull", "disabled", Some("b"), Some("t")),
            NotPullable::Disabled
        );
    }

    #[test]
    fn each_missing_coordinate_is_named_individually() {
        // 两项各自要有自己的变体：只说「坐标不全」等于让人回去逐个猜。
        assert_eq!(
            refusal("pull", "active", None, Some("t")),
            NotPullable::MissingBaseToken
        );
        assert_eq!(
            refusal("pull", "active", Some("b"), None),
            NotPullable::MissingTableId
        );
    }

    #[test]
    fn blank_coordinates_count_as_missing() {
        // 空串与 NULL 在库里是两种形态，但对「能不能拉」是同一件事——
        // `load_pull_tables` 也是 trim 之后判空的。
        assert_eq!(
            refusal("pull", "active", Some("   "), Some("t")),
            NotPullable::MissingBaseToken
        );
    }

    #[test]
    fn a_fully_configured_pull_table_passes_the_check() {
        assert!(check_pullable("pull", "active", Some("b"), Some("t")).is_ok());
    }

    #[test]
    fn every_reason_says_what_to_fix() {
        // 文案是这一层的**唯一**产出——运维拿着它回去改配置。
        // 空文案等于把「不可拉取」原样丢回去。
        for reason in [
            NotPullable::PushMode,
            NotPullable::Disabled,
            NotPullable::MissingBaseToken,
            NotPullable::MissingTableId,
        ] {
            assert!(!reason.reason().trim().is_empty(), "{reason:?} 缺文案");
        }
    }

    // -----------------------------------------------------------------------
    // 表级拉取：纯函数部分（设计 §6.2）
    // -----------------------------------------------------------------------

    /// 一条已解析过名字的绑定：`field_id` 与它的当前 `field_name` 都在手边。
    fn bound(field_id: &str, name: &str, parent: Option<&str>) -> BoundField {
        BoundField {
            field_id: field_id.to_string(),
            field_name: name.to_string(),
            parent_field_id: parent.map(str::to_string),
        }
    }

    /// 一条落库视角的绑定。`source_key` 由 `field_id` 派生，只有需要断言它时才单写。
    fn table_binding(field_id: &str, name: Option<&str>, parent: Option<&str>) -> TableBinding {
        TableBinding {
            id: 1,
            source_key: format!("src_{field_id}"),
            field_id: field_id.to_string(),
            field_name: name.map(str::to_string),
            parent_field_id: parent.map(str::to_string),
            snapshot_digest: None,
        }
    }

    #[test]
    fn field_names_are_the_union_and_are_deduped() {
        // field_names 是一个 JSON 数组查询参数，重复项会被官方拒
        // （bitable.rs 的 duplicate_field_names_are_rejected 钉着这条）
        let fields = vec![
            bound("fldA", "币种/Currency（单选）", None),
            bound("fldB", "汇率/Exchange Rate", Some("fldA")),
        ];
        let names = collect_field_names(&fields);
        assert_eq!(names, vec!["币种/Currency（单选）", "汇率/Exchange Rate"]);
    }

    #[test]
    fn a_column_that_is_both_a_value_and_a_parent_is_listed_once() {
        // 中间的父列（如 费用类型）既自己取值、又是别人的父 → 只能出现一次
        let fields = vec![
            bound("fldP", "费用大类/Main Exp Cat*", None),
            bound("fldM", "费用类型/Fee Type*", Some("fldP")),
            bound("fldC", "银行流水摘要-编码", Some("fldM")),
        ];
        let names = collect_field_names(&fields);
        assert_eq!(names.len(), 3, "三级链的中间列不得重复: {names:?}");
    }

    #[test]
    fn a_cyclic_pair_does_not_duplicate_a_name() {
        // 建源时已挡环（T5），但拼 field_names 的函数不能依赖上游一定挡住了
        let fields = vec![
            bound("fldA", "甲", Some("fldB")),
            bound("fldB", "乙", Some("fldA")),
        ];
        assert_eq!(collect_field_names(&fields).len(), 2);
    }

    #[test]
    fn parent_and_child_labels_are_trimmed_identically() {
        // 实测该表有 "CNY 人民币\n" 这种尾随换行。父键与子键必须用同一份 trim 后的文案
        // （derive.rs 的两处都 trim），否则子项指向一个不存在的父 option_id
        // ——失效形态是「下拉静默变空」，不报错。
        let parent_rows = [RawValue {
            parent_label: None,
            label: "CNY 人民币\n",
        }];
        let child_rows = [
            RawValue {
                parent_label: Some("CNY 人民币\n"),
                label: "1.0000",
            },
            RawValue {
                parent_label: Some("CNY 人民币"),
                label: "1.0000",
            },
        ];
        let parent = derive_options("currency", None, &parent_rows);
        let child = derive_options("fx", Some("currency"), &child_rows);
        assert_eq!(parent.len(), 1, "两种写法折叠成同一个父选项");
        assert_eq!(child.len(), 1, "同父同文案只派生一个子选项");
        assert_eq!(
            child[0].parent_key, parent[0].option_id,
            "子键必须命中父的 option_id"
        );
    }

    #[test]
    fn a_row_with_a_parent_but_no_child_value_yields_no_option() {
        // 设计 Review Focus 第 4 条：父列有值、子列为空 与 父列为空 是两种情形，
        // 但都不得挂到一个空父上。
        let rows = [
            RawValue {
                parent_label: Some("推广测评服务费"),
                label: "   ",
            },
            RawValue {
                parent_label: Some("   "),
                label: "pay for services-X",
            },
        ];
        let derived = derive_options("summary", Some("fee_type"), &rows);
        assert_eq!(derived.len(), 1, "空文案不产出选项（derive.rs 的 trim）");
        assert_eq!(derived[0].parent_key, "", "父文案为空时落无父键，不挂空父");
    }

    #[test]
    fn the_same_label_under_two_parents_yields_two_options() {
        // 目标表实测有 6 例「一子多父」（如 pay for services-YL-AR 同时属于
        // 推广测评服务费 与 预付储值款）。设计答案：各派生一个，互不覆盖。
        let rows = [
            RawValue {
                parent_label: Some("推广测评服务费"),
                label: "pay for services-YL-AR",
            },
            RawValue {
                parent_label: Some("预付储值款"),
                label: "pay for services-YL-AR",
            },
        ];
        let derived = derive_options("summary", Some("fee_type"), &rows);
        assert_eq!(derived.len(), 2, "同文案不同父必须派生两个选项");
        assert_ne!(derived[0].parent_key, derived[1].parent_key);
    }

    #[test]
    fn the_targets_cover_bound_columns_and_their_parents() {
        // 父列也要解析：快照里靠**名字**取父列的值，拼不出父键的子树会整棵塌掉
        let bindings = vec![
            table_binding("fldA", Some("币种"), None),
            table_binding("fldB", Some("汇率"), Some("fldA")),
        ];
        assert_eq!(resolve_targets(&bindings), vec!["fldA", "fldB"]);
    }

    #[test]
    fn a_blank_parent_pointer_is_not_a_target() {
        // 界面清空父列时可能提交空串而不是 null；当成「没有父」，
        // 而不是拿一个空 id 去解析（那会让整表因为一个不存在的字段停摆）
        let bindings = vec![table_binding("fldA", Some("币种"), Some("   "))];
        assert_eq!(resolve_targets(&bindings), vec!["fldA"]);
        let bound = bind_fields(&bindings, &[("fldA".to_string(), "币种".to_string())])
            .unwrap_or_else(|error| panic!("应可绑定: {error}"));
        assert!(bound[0].parent_field_id.is_none());
    }

    #[test]
    fn a_binding_whose_parent_is_not_enabled_fails_the_whole_table() {
        // 父列不在启用集合里 → 读不到父列文案，快照里也没有它 → 子树 option_id 整棵变，
        // 飞书控件上已选中的值全部失联**且不报错**。所以整体失败并点名（D4 全有或全无）。
        let bindings = vec![
            table_binding("fldA", Some("币种"), None),
            table_binding("fldB", Some("汇率"), Some("fldDisabled")),
        ];
        let resolved = vec![
            ("fldA".to_string(), "币种".to_string()),
            ("fldB".to_string(), "汇率".to_string()),
        ];
        let Err(error) = bind_fields(&bindings, &resolved) else {
            panic!("父列不在启用集合里必须整体失败，不能静默降级成无父");
        };
        assert!(
            error.to_string().contains("fldDisabled"),
            "错误里要点名缺失的父列: {error}"
        );
    }

    #[test]
    fn the_parent_linkage_uses_the_resolved_name_not_the_cache() {
        // 父列改名后，缓存里的旧名字在快照里读不到任何值 → 全部子项静默变成无父。
        // 所以父键必须用**本轮解析出来的**名字。
        let bindings = vec![
            TableBinding {
                id: 7,
                source_key: "currency".to_string(),
                field_id: "fldA".to_string(),
                field_name: Some("旧名字".to_string()),
                parent_field_id: None,
                snapshot_digest: None,
            },
            table_binding("fldB", Some("汇率"), Some("fldA")),
        ];
        let resolved = vec![
            ("fldA".to_string(), "币种（改名后）".to_string()),
            ("fldB".to_string(), "汇率".to_string()),
        ];
        let bound =
            bind_fields(&bindings, &resolved).unwrap_or_else(|error| panic!("应可绑定: {error}"));
        let linkage = parent_linkage(&bound[1], &bindings, &bound)
            .unwrap_or_else(|error| panic!("应可解析: {error}"))
            .unwrap_or_else(|| panic!("fldB 声明了父列"));
        assert_eq!(linkage.parent_source_key, "currency");
        assert_eq!(linkage.parent_field, "币种（改名后）");
    }

    #[test]
    fn a_binding_without_a_parent_has_no_linkage() {
        let bindings = vec![table_binding("fldA", Some("币种"), None)];
        let bound = bind_fields(&bindings, &[("fldA".to_string(), "币种".to_string())])
            .unwrap_or_else(|error| panic!("应可绑定: {error}"));
        let linkage = parent_linkage(&bound[0], &bindings, &bound)
            .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert!(linkage.is_none());
    }

    #[test]
    fn an_empty_name_cache_is_stale() {
        // 首次拉取时缓存是空的。那不是「改名」，但名字必须写回去，否则控制台一直显示空。
        let fresh = table_binding("fldA", None, None);
        assert!(name_cache_is_stale(&fresh, "币种"));
        let same = table_binding("fldA", Some("币种"), None);
        assert!(!name_cache_is_stale(&same, "币种"));
    }

    #[test]
    fn a_trimmed_name_is_not_a_rename() {
        // 解析结果与缓存都是 trim 过的，两侧口径必须一致——否则每轮都白开一个事务
        let renamed = table_binding("fldA", Some("币种（改名后）"), None);
        assert!(name_cache_is_stale(&renamed, "币种"));
    }
}
