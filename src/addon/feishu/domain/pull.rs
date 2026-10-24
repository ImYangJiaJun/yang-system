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
    cell_label, is_option_source_field_type, list_all_fields, list_all_records, BitableCoordinates,
    CellValue, FieldItem, RecordsSnapshot,
};
use super::context::FeishuContext;
use super::derive::{derive_options, snapshot_digest, DerivedOption, RawValue};
use super::linkage::{Linkage, LinkageLevel};
use super::option_write::{
    apply_option_rows, count_option_rows, disable_option_rows, find_foreign_option_owner,
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
/// 「父列不在集合内」由 [`partition_bindings`] 标成跳过，不在这里静默降级。
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

/// 一轮表级拉取的结果状态。
///
/// 失败（`Err`）不在这里：整轮失败由 worker 记成 `failed`。这里只区分
/// 「全部字段都处理了」与「有字段被跳过、其余字段仍处理了」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PullStatus {
    #[default]
    Success,
    Partial,
}

impl PullStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Partial => "partial",
        }
    }
}

/// 一条绑定本轮被跳过的原因。
///
/// 每种原因对应不同的修法，所以不合并成一个笼统的「失败」：界面给业务人员看的
/// 指引必须能指向「去多维表格把这一列补回来 / 改成唯一列名 / 换一列」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// 远端字段 ID 已不存在，或字段名为空。
    MissingField,
    /// 字段名在表内不唯一，按名字取数会取到不确定的列。
    DuplicateName,
    /// 字段类型不能作为选项值（人员、附件、关联、群组、复选框、地理位置等）。
    UnsupportedType,
    /// 多选列被用作级联父列：父键需要唯一文案，多选一格可能有多个值。
    MultiValueParent,
    /// 父绑定不在本表的启用绑定里（被停用或删除）。
    ParentUnavailable,
    /// 父绑定本轮也被跳过了：整棵子树必须一起暂停，否则子选项 id 会静默变化。
    ParentSkipped,
    /// 父指针成环。
    ParentCycle,
}

impl SkipReason {
    /// 给人看的一句话。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::MissingField => "多维表格里已找不到这个字段（可能已被删除，或字段名为空）",
            Self::DuplicateName => "字段名在表内不唯一，按名字取数会取到不确定的列",
            Self::UnsupportedType => "字段类型不支持作为选项值",
            Self::MultiValueParent => {
                "多选字段只能作为叶子取数列，不能作为级联父列（父键需要唯一文案）"
            }
            Self::ParentUnavailable => "父字段不在本表的启用绑定里（已停用或已删除）",
            Self::ParentSkipped => "父字段本轮也被跳过，子字段一起暂停",
            Self::ParentCycle => "父指针成环",
        }
    }
}

/// 一条本轮被跳过的绑定。摘要会写进表级 `last_error`，也会展示在体检/控制台。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedBinding {
    pub(crate) source_key: String,
    pub(crate) field_id: String,
    /// 绑定行上的名字缓存；远端字段被删后，这是**最后一次成功解析出来的名字**。
    pub(crate) field_name: Option<String>,
    pub(crate) reason: SkipReason,
}

impl SkippedBinding {
    /// 业务人员看的名字：缓存优先，没有名字才退回 ID。
    pub(crate) fn display_name(&self) -> &str {
        self.field_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.field_id)
    }
}

/// 把一组被跳过的绑定压成一句可落库、可展示的摘要。
///
/// 必须点名**字段名**：业务人员看不懂 `field_id`。ID 与 `source_key` 作为次要信息
/// 放在括号里，供运维定位。
pub(crate) fn skipped_summary(skipped: &[SkippedBinding]) -> String {
    let details: Vec<String> = skipped
        .iter()
        .map(|item| {
            format!(
                "字段「{}」（字段 ID {}，标识 {}）：{}",
                item.display_name(),
                item.field_id,
                item.source_key,
                item.reason.label()
            )
        })
        .collect();
    format!(
        "本轮跳过 {} 个字段，其余字段照常同步：{}",
        details.len(),
        details.join("；")
    )
}

/// 一张表一轮拉取的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TablePullOutcome {
    /// 整轮状态：全部处理 vs 部分字段被跳过。
    pub(crate) status: PullStatus,
    /// 本轮被跳过的绑定（按原绑定顺序）。
    pub(crate) skipped_bindings: Vec<SkippedBinding>,
    /// 拉到的记录行数。**一份快照服务整表**，所以这是表级数字。
    pub(crate) fetched: usize,
    /// 本轮**实际处理**的字段绑定条数（不含被跳过的）。
    pub(crate) fields: usize,
    /// 内容未变而跳过写库的字段数。
    pub(crate) unchanged: usize,
    /// 写入路径 `rows_affected` 之和，**不是行数**（多行 ODKU 的 `rows_affected`
    /// ∈ `[N,2N]`，也分不出 inserted/updated；口径见 `option_write` 模块文档）。
    pub(crate) rows_affected: u64,
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
    /// 取数方式不是 `pull`：`push`（等飞书多维表格自动化来推）或 `xlsx_import`
    /// （数据来自控制台上传的文件）。两种都不走服务端定时出网这条路径。
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
            Self::PushMode => {
                "取数方式是「手工推送」或「文件导入」，不是「定时拉取」——\
                 服务端只对「定时拉取」的数据源出网拉数"
            }
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
/// # 字段级隔离（本次需求 D1）
///
/// 远端字段缺失、重名、类型不支持、父链不可用时，**只跳过受影响的绑定及其子树**，
/// 其余绑定照常派生落库。跳过的原因与字段名汇总进 `last_error`，表级状态为
/// `partial`；整轮没有任何可处理字段时才落成失败。
///
/// # 状态列的归属（设计 §6.3）
///
/// `snapshot_digest` 落在**字段绑定**行——一个字段内容没变就该跳过它的写库；
/// `last_pull_at` / `last_success_at` / `consecutive_failures` / `last_error` /
/// `last_pull_status` 落在**表级**行。状态写入由调用方（worker）统一收口：
/// 成功、部分成功、失败三条路径各自写一次，避免两边都写导致计数翻倍。
///
/// 一张表启用中的绑定为空时直接返回：它本来就没有要与飞书同步的东西。
pub(crate) async fn pull_table(
    deps: &PullDeps<'_>,
    table: &PullTable,
) -> Result<TablePullOutcome, BaseError> {
    pull_table_inner(deps, table).await
}

/// 表级编排的本体。
async fn pull_table_inner(
    deps: &PullDeps<'_>,
    table: &PullTable,
) -> Result<TablePullOutcome, BaseError> {
    let mut outcome = TablePullOutcome::default();
    if table.bindings.is_empty() {
        return Ok(outcome);
    }

    // 1) 一次元数据往返。与旧的「全有或全无」不同：缺失/重名/类型不支持不再让整表
    //    失败，而是把受影响的绑定（及其子树）标成 skipped，其余字段继续。
    let fields = list_all_fields(
        deps.transport,
        deps.sleeper,
        deps.tokens,
        &table.coordinates,
    )
    .await
    .map_err(internal_error)?;
    let PartitionResult {
        healthy_indices,
        bound,
        skipped,
    } = partition_bindings(&table.bindings, &fields);
    outcome.skipped_bindings = skipped;

    if healthy_indices.is_empty() {
        // 一条能处理的都没有：这不是「部分成功」，而是整表本轮无法同步。
        // 返回 Err 让 worker 记 failed 并告警；跳过清单已经点名每一列。
        return Err(BaseError::ConfigError(skipped_summary(
            &outcome.skipped_bindings,
        )));
    }

    // 2) 一次取回一份快照，服务整表所有**可处理**字段——这是表级拉取的全部意义。
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
    //    可处理字段本地留着已启用选项，整张表本轮一个写都不做（理由见模块文档）。
    if snapshot.items.is_empty() {
        let mut suspicious = Vec::new();
        for index in &healthy_indices {
            let binding = &table.bindings[*index];
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

    // 4) 逐条**可处理**绑定：抽取 → 派生 → 比对 → 落库（每条一个事务）。
    for (index, field) in healthy_indices.iter().zip(bound.iter()) {
        let binding = &table.bindings[*index];
        outcome.fields += 1;
        let linkage = parent_linkage(field, &table.bindings, &bound)?;
        // 只有多选列才按元素展开；其余列（含公式/查找引用）返回多值仍按旧的
        // 「取数列必须是单值」规则跳过，避免悄悄扩大取值口径。
        let field_type = fields
            .iter()
            .find(|remote| remote.field_id == field.field_id)
            .and_then(|remote| remote.field_type);
        let values =
            extract_values_owned(&snapshot, &field.field_name, field_type, linkage.as_ref());
        // 派生要 `&[RawValue]`，而 `RawValue.ancestors` 是借用切片——先把每行的祖先引用
        // 算好（它活到本语句块末），再借出去；派生完即丢。
        let ancestor_refs: Vec<Vec<&str>> = values
            .iter()
            .map(|value| value.ancestors.iter().map(String::as_str).collect())
            .collect();
        let raw: Vec<RawValue<'_>> = values
            .iter()
            .zip(ancestor_refs.iter())
            .map(|(value, refs)| value.as_raw(refs))
            .collect();
        let derived = derive_options(
            &binding.source_key,
            &linkage
                .as_ref()
                .map(Linkage::source_keys)
                .unwrap_or_default(),
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
            outcome.unchanged += 1;
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
        outcome.rows_affected += written.rows_affected;
        outcome.disabled += written.disabled;

        tracing::debug!(
            source_key = %binding.source_key,
            fetched = outcome.fetched,
            derived = derived.len(),
            rows_affected = written.rows_affected,
            disabled = written.disabled,
            "飞书数据源字段同步完成"
        );
    }

    outcome.status = if outcome.skipped_bindings.is_empty() {
        PullStatus::Success
    } else {
        PullStatus::Partial
    };
    Ok(outcome)
}

/// 一条绑定本轮落库的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BindingWrite {
    /// 选项写入的 `rows_affected`（口径见 [`TablePullOutcome::rows_affected`]）。
    rows_affected: u64,
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
            let rows: Vec<Record> = plan
                .derived
                .iter()
                .map(|option| to_option_row(&binding.source_key, option))
                .collect();
            written.rows_affected =
                apply_option_rows(options, &mut transaction, &binding.source_key, rows).await?;
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
pub(crate) async fn record_table_success(
    deps: &PullDeps<'_>,
    table_id: i64,
) -> Result<(), BaseError> {
    let mut transaction = deps.database.transaction().await?;
    let mut update = Record::new();
    update.insert("last_pull_at", serde_json::json!(now_seconds()));
    update.insert("last_success_at", serde_json::json!(now_seconds()));
    update.insert("consecutive_failures", serde_json::json!(0));
    update.insert("last_error", serde_json::Value::Null);
    update.insert("last_pull_status", serde_json::json!("success"));
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

/// 记录一次**部分成功**：`consecutive_failures` 自增、写 `last_error` 与
/// `last_pull_status = partial`，返回自增后的次数。
///
/// 部分成功仍然是一次「有字段没同步」的失败，所以沿用同一个连续失败计数与告警阈值：
/// 否则一个被删字段可以永远静默地停更，而告警系统毫无反应。区别只在状态与文案——
/// 健康字段已经写库，`last_success_at` **不**推进（它不是整表成功）。
pub(crate) async fn record_table_partial(
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
        tracing::warn!(table_id, "表级数据源已不存在，本轮部分成功状态无处记账");
        return Ok(0);
    };
    let failures: i64 = current.optional("consecutive_failures")?.unwrap_or(0);
    let message: String = message.chars().take(1000).collect();

    let mut transaction = deps.database.transaction().await?;
    let mut update = Record::new();
    update.insert("consecutive_failures", serde_json::json!(failures + 1));
    update.insert("last_pull_at", serde_json::json!(now_seconds()));
    update.insert("last_error", serde_json::json!(message));
    update.insert("last_pull_status", serde_json::json!("partial"));
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
    update.insert("last_pull_status", serde_json::json!("failed"));
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

/// 一张表的绑定分区结果。
struct PartitionResult {
    /// 可处理绑定在 `bindings` 里的下标（保序）。
    healthy_indices: Vec<usize>,
    /// 与 `healthy_indices` 一一对应的解析结果。
    bound: Vec<BoundField>,
    /// 本轮跳过的绑定（保序，带字段名缓存与原因）。
    skipped: Vec<SkippedBinding>,
}

/// 在绑定集合里按 `field_id` 找下标。
///
/// 绑定表没有 `(datasource_id, field_id)` 唯一索引，手工改库可能造出重复行；
/// 这里取第一条，与旧实现的 `find` 口径一致。新增/改绑入口自己会挡重复。
fn parent_index(bindings: &[TableBinding], field_id: &str) -> Option<usize> {
    bindings
        .iter()
        .position(|binding| binding.field_id == field_id)
}

/// 父绑定被跳过时，把整棵子树也标成跳过。
///
/// 不能把「父被跳过」静默当成「无父」：子选项的 `option_id` 含祖先链哈希，
/// 静默改父键会让整棵子树换 id，飞书已选中的值全部失联且不报错。
fn propagate_parent_skips(bindings: &[TableBinding], skipped: &mut HashMap<usize, SkipReason>) {
    loop {
        let mut changed = false;
        for (index, binding) in bindings.iter().enumerate() {
            if skipped.contains_key(&index) {
                continue;
            }
            let Some(parent) = declared_parent(binding) else {
                continue;
            };
            let Some(parent_index) = parent_index(bindings, parent) else {
                continue;
            };
            if skipped.contains_key(&parent_index) {
                skipped.insert(index, SkipReason::ParentSkipped);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// 把绑定分成「可处理」与「跳过」两组。
///
/// 判据与旧的 `resolve_field_names`/`bind_fields` 同源，但**失败粒度从整表降到绑定**：
/// - 远端找不到 `field_id`、或字段名为空 → `MissingField`
/// - 字段名在表内不唯一 → `DuplicateName`（按名字取数会取到不确定的列）
/// - 字段类型不能作为选项值 → `UnsupportedType`
/// - 多选列被用作级联父列 → `MultiValueParent`（多选列只能作为叶子取数列）
/// - 父绑定不在启用集合里 → `ParentUnavailable`
/// - 父绑定被跳过 → `ParentSkipped`（整棵子树一起暂停）
/// - 父指针成环 → `ParentCycle`
///
/// 只跳过受影响的绑定及其子树；无关分支照常同步。
fn partition_bindings(bindings: &[TableBinding], fields: &[FieldItem]) -> PartitionResult {
    let mut remote_by_id: HashMap<&str, &FieldItem> = HashMap::new();
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for field in fields {
        remote_by_id.insert(field.field_id.as_str(), field);
        let name = field.field_name.trim();
        if !name.is_empty() {
            *name_counts.entry(name).or_insert(0) += 1;
        }
    }

    let mut skipped: HashMap<usize, SkipReason> = HashMap::new();
    for (index, binding) in bindings.iter().enumerate() {
        let Some(remote) = remote_by_id.get(binding.field_id.as_str()) else {
            skipped.insert(index, SkipReason::MissingField);
            continue;
        };
        let name = remote.field_name.trim();
        if name.is_empty() {
            skipped.insert(index, SkipReason::MissingField);
            continue;
        }
        if name_counts.get(name).copied().unwrap_or(0) > 1 {
            skipped.insert(index, SkipReason::DuplicateName);
            continue;
        }
        if remote
            .field_type
            .is_some_and(|field_type| !is_option_source_field_type(field_type))
        {
            skipped.insert(index, SkipReason::UnsupportedType);
            continue;
        }
        // 多选列可以当选项来源（逐元素展开），但不能当父列：父键按一格一个文案
        // 折叠，多值父会让子项挂到不确定的父上。父被跳过时，子树由下面的传播一并暂停。
        if remote.field_type == Some(4)
            && bindings
                .iter()
                .any(|other| declared_parent(other) == Some(remote.field_id.as_str()))
        {
            skipped.insert(index, SkipReason::MultiValueParent);
        }
    }

    // 父绑定不在启用集合里：直接标 ParentUnavailable；父已被跳过则标 ParentSkipped。
    for (index, binding) in bindings.iter().enumerate() {
        if skipped.contains_key(&index) {
            continue;
        }
        let Some(parent) = declared_parent(binding) else {
            continue;
        };
        match parent_index(bindings, parent) {
            None => {
                skipped.insert(index, SkipReason::ParentUnavailable);
            }
            Some(parent_index) if skipped.contains_key(&parent_index) => {
                skipped.insert(index, SkipReason::ParentSkipped);
            }
            Some(_) => {}
        }
    }
    propagate_parent_skips(bindings, &mut skipped);

    // 成环：沿父链走，第一次回到路径上的节点时，只把环上的节点标成 ParentCycle；
    // 环前面的节点由下一轮传播标成 ParentSkipped。
    for start in 0..bindings.len() {
        if skipped.contains_key(&start) {
            continue;
        }
        let mut path: Vec<usize> = Vec::new();
        let mut cursor = Some(start);
        while let Some(index) = cursor {
            if let Some(position) = path.iter().position(|&seen| seen == index) {
                for &member in &path[position..] {
                    skipped.insert(member, SkipReason::ParentCycle);
                }
                break;
            }
            path.push(index);
            cursor =
                declared_parent(&bindings[index]).and_then(|parent| parent_index(bindings, parent));
        }
    }
    propagate_parent_skips(bindings, &mut skipped);

    let mut result = PartitionResult {
        healthy_indices: Vec::new(),
        bound: Vec::new(),
        skipped: Vec::new(),
    };
    for (index, binding) in bindings.iter().enumerate() {
        if let Some(reason) = skipped.get(&index).copied() {
            result.skipped.push(SkippedBinding {
                source_key: binding.source_key.clone(),
                field_id: binding.field_id.clone(),
                field_name: binding.field_name.clone(),
                reason,
            });
            continue;
        }
        let Some(remote) = remote_by_id.get(binding.field_id.as_str()) else {
            // 防守性兜底：理论上不会走到，仍然点名而不是 panic。
            result.skipped.push(SkippedBinding {
                source_key: binding.source_key.clone(),
                field_id: binding.field_id.clone(),
                field_name: binding.field_name.clone(),
                reason: SkipReason::MissingField,
            });
            continue;
        };
        result.healthy_indices.push(index);
        result.bound.push(BoundField {
            field_id: binding.field_id.clone(),
            field_name: remote.field_name.trim().to_string(),
            parent_field_id: declared_parent(binding).map(str::to_string),
        });
    }
    result
}

/// 一条绑定的祖先链：从**根到直接父**，每级给出「父的 `source_key`」与「父的**当前**名字」。
///
/// 沿 `parent_field_id` 反复上溯，所以任意深度都能拼出完整一条链。每级的名字取
/// **解析后**的那一份，不能用绑定行上的缓存——缓存是上一轮的名字，父列改名后按旧名字
/// 读快照会一片空白（`record.fields.get` 取不到键，整棵子树的父键一起塌成空）。
///
/// # 为什么必须防环
///
/// `validate_fields` 拒自环与缺失父，但**手工改库能造出 a→b→a**。上溯不设 visited
/// 守卫的话，一轮拉取会在这里**卡死**——不是报错，是 worker 再也不返回。卡死比报错
/// 难查得多，所以这里宁可直接失败并点名。
///
/// # 为什么是 `pub(crate)`
///
/// 文件导入（`datasource/actions/import_xlsx.rs`）的祖先链与拉取是同一件事、同一组
/// 输入，所以它复用本函数而不是再写一份——**再写一份等于把上面那条防环守卫复制两遍**，
/// 手工改库造出的环会在一处被挡、在另一处把请求卡死。
pub(crate) fn parent_linkage(
    field: &BoundField,
    bindings: &[TableBinding],
    bound: &[BoundField],
) -> Result<Option<Linkage>, BaseError> {
    let Some(first_parent) = field.parent_field_id.as_deref() else {
        return Ok(None);
    };

    let mut chain: Vec<LinkageLevel> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    visited.insert(field.field_id.as_str());
    let mut next = Some(first_parent);

    while let Some(parent_field_id) = next {
        if !visited.insert(parent_field_id) {
            return Err(BaseError::ConfigError(format!(
                "字段绑定 {} 的父指针成环（回到 {parent_field_id}）：上溯停不下来，本轮整表停止",
                field.field_id
            )));
        }
        let (Some(parent_binding), Some(parent_field)) = (
            bindings
                .iter()
                .find(|other| other.field_id == parent_field_id),
            bound.iter().find(|other| other.field_id == parent_field_id),
        ) else {
            // `partition_bindings` 已经挡过一次；这是同一条判据的第二道，防的是将来
            // 有人绕过它单独调用本函数（xlsx 导入就复用本函数）。
            return Err(BaseError::ConfigError(format!(
                "字段绑定 {} 的父列 {parent_field_id} 不在本表的启用绑定里",
                field.field_id
            )));
        };
        chain.push(LinkageLevel {
            source_key: parent_binding.source_key.clone(),
            field_name: parent_field.field_name.clone(),
        });
        next = parent_field.parent_field_id.as_deref();
    }

    // 上溯得到的是「直接父 → 根」，而派生要「根 → 直接父」：折叠必须从根往下。
    chain.reverse();
    Ok(Some(Linkage { ancestors: chain }))
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
    field_type: Option<i32>,
    linkage: Option<&Linkage>,
) -> Vec<OwnedRawValue> {
    let mut values = Vec::new();
    let mut unsupported = 0usize;
    for record in &snapshot.items {
        let Some(value) = record.fields.get(field_name) else {
            continue;
        };
        let labels = match cell_label(value) {
            CellValue::Text(labels) => labels,
            CellValue::Empty => continue,
            CellValue::Unsupported => {
                unsupported += 1;
                continue;
            }
        };
        // 旧的「取数列必须是单值」规则只对多选列放宽：其他列（尤其公式/查找）
        // 返回多值时仍然跳过并告警，不把取值口径悄悄放大。
        if field_type != Some(4) && labels.len() > 1 {
            unsupported += 1;
            continue;
        }
        // 每一级祖先各读**同一条 record** 里那一列（A7b：同行共现，不去父表反查）。
        // 取不到（列不在快照里）或不是单值文本（人员/地理位置/多选父列）都记成**空串**，
        // 由派生的两条空级规则处理：直接父为空 ⇒ 本行无父；中间级为空 ⇒ 跳过那一级。
        // 多选父列在 `partition_bindings` 已被跳过，这里的 `len() != 1` 只是防守。
        let ancestors = match linkage {
            None => Vec::new(),
            Some(linkage) => linkage
                .ancestors
                .iter()
                .map(|level| {
                    record
                        .fields
                        .get(&level.field_name)
                        .and_then(|value| match cell_label(value) {
                            CellValue::Text(mut labels) if labels.len() == 1 => labels.pop(),
                            _ => None,
                        })
                        .unwrap_or_default()
                })
                .collect(),
        };
        // 多选列的每个元素各派生一个选项；同一格共享该记录（同行）的祖先链。
        for label in labels {
            values.push(OwnedRawValue {
                ancestors: ancestors.clone(),
                label,
            });
        }
    }
    if unsupported > 0 {
        // 非空告警：静默丢弃会让「取数列选错类型」表现为「选项莫名变少」。
        tracing::warn!(
            unsupported,
            field_name,
            "取数列存在取值不支持的单元格，已跳过（人员/地理位置等无文本类型，或非多选列返回了多个值）"
        );
    }
    values
}

/// [`RawValue`] 的自有版本。`ancestors` 从根到直接父，取不到的那一级是空串。
#[derive(Debug, Clone)]
struct OwnedRawValue {
    ancestors: Vec<String>,
    label: String,
}

impl OwnedRawValue {
    /// 借出成 [`RawValue`]。
    ///
    /// `ancestor_refs` 必须与 `self.ancestors` 等长：`RawValue.ancestors` 是一个**借用的
    /// 切片**，而在 `as_raw` 里现算的话活不过返回值，所以由调用方先算好再传进来。
    fn as_raw<'a>(&'a self, ancestor_refs: &'a [&'a str]) -> RawValue<'a> {
        RawValue {
            ancestors: ancestor_refs,
            label: self.label.as_str(),
        }
    }
}

/// 把派生结果转成待写入的行（**整行替换**：把本轮派生的全部列一并写）。
///
/// `enabled` 必须显式写 `true`：出站拉取是整行替换，某行被补集停用后又重新出现时
/// 要能复活。这与入站 upsert 的「合并」语义不同，也是本函数不复用 `to_record` 的原因。
///
/// 列集**全批同一份**（每个 `DerivedOption` 都走这里），这是批量 upsert 的硬前提；
/// `option_id`/`source_key` 由 `apply_option_rows` 从赋值列里剔除（身份列，见其文档）。
fn to_option_row(source_key: &str, option: &DerivedOption) -> Record {
    let mut record = Record::new();
    record.insert("option_id", serde_json::json!(option.option_id));
    record.insert("source_key", serde_json::json!(source_key));
    record.insert("label", serde_json::json!(option.label));
    record.insert("sort_order", serde_json::json!(option.sort_order));
    record.insert("enabled", serde_json::json!(true));
    record.insert("parent_key", serde_json::json!(option.parent_key));
    record.insert("last_push_at", serde_json::json!(now_seconds()));
    record
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
                    created_by: None,
                }
            })
            .collect::<Vec<_>>();
        RecordsSnapshot {
            total: items.len() as i64,
            items,
        }
    }

    /// 一级祖先的级联声明。`cascade_field` 已随「形状收敛到 `domain/linkage.rs`」
    /// 去掉（它零消费）；2026-09-27 又把「一个父」扩成「一串祖先」，见该模块文档。
    fn linkage() -> Linkage {
        Linkage {
            ancestors: vec![LinkageLevel {
                source_key: "currency".to_string(),
                field_name: "父".to_string(),
            }],
        }
    }

    #[test]
    fn values_pair_child_with_the_same_rows_parent() {
        // 父键由**同行共现**读出，这是 A7b 的依据
        let snap = snapshot(vec![("6.9025", "USD 美元"), ("0.8807", "HKD 港币")]);
        let values = extract_values_owned(&snap, "子", Some(3), Some(&linkage()));
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].label, "6.9025");
        assert_eq!(values[0].ancestors, vec!["USD 美元"]);
        assert_eq!(values[1].ancestors, vec!["HKD 港币"]);
    }

    #[test]
    fn without_linkage_parents_are_absent() {
        let snap = snapshot(vec![("甲", "父值")]);
        let values = extract_values_owned(&snap, "子", Some(3), None);
        assert_eq!(values.len(), 1);
        assert!(values[0].ancestors.is_empty());
    }

    #[test]
    fn empty_cells_do_not_produce_values() {
        let snap = snapshot(vec![("", "USD"), ("6.9025", "USD")]);
        let values = extract_values_owned(&snap, "子", Some(3), Some(&linkage()));
        assert_eq!(values.len(), 1, "空单元格不贡献任何值");
        assert_eq!(values[0].label, "6.9025");
    }

    #[test]
    fn missing_child_column_yields_nothing() {
        // 取数列名配错时不是报错，而是没有任何值——上层会看到 derived=0
        let snap = snapshot(vec![("甲", "父值")]);
        assert!(extract_values_owned(&snap, "不存在的列", Some(3), None).is_empty());
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
                created_by: None,
            }],
        };
        assert!(extract_values_owned(&snap, "子", Some(11), None).is_empty());
    }

    fn snapshot_with(
        fields: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> RecordsSnapshot {
        RecordsSnapshot {
            total: 1,
            items: vec![RecordItem {
                record_id: "rec".to_string(),
                fields,
                created_by: None,
            }],
        }
    }

    #[test]
    fn multi_select_cells_yield_one_value_per_label_sharing_the_rows_ancestor_chain() {
        // 一条记录带两个附件类型：两个选项，父链都取同一条记录的父列。
        let fields = std::collections::BTreeMap::from([
            ("子".to_string(), json!(["A", "B"])),
            ("父".to_string(), json!("P")),
        ]);
        let snap = snapshot_with(fields);
        let values = extract_values_owned(&snap, "子", Some(4), Some(&linkage()));
        assert_eq!(values.len(), 2, "多选列的每个元素各产一个值");
        assert_eq!(values[0].label, "A");
        assert_eq!(values[0].ancestors, vec!["P"]);
        assert_eq!(values[1].label, "B");
        assert_eq!(values[1].ancestors, vec!["P"], "同一格的元素共享同行祖先链");
    }

    #[test]
    fn empty_multi_select_cells_contribute_no_values() {
        for cell in [json!([]), json!(["", "   "])] {
            let fields = std::collections::BTreeMap::from([("子".to_string(), cell)]);
            assert!(extract_values_owned(&snapshot_with(fields), "子", Some(4), None).is_empty());
        }
    }

    #[test]
    fn a_mixed_multi_select_cell_is_skipped_as_a_whole() {
        // 混入不可识别元素时不能「丢一半、留一半」。
        let fields = std::collections::BTreeMap::from([(
            "子".to_string(),
            json!(["A", {"id": "ou_x", "name": "张三"}]),
        )]);
        assert!(extract_values_owned(&snapshot_with(fields), "子", Some(4), None).is_empty());
    }

    #[test]
    fn a_non_multi_select_field_with_multiple_labels_is_still_skipped() {
        // 公式/查找列也可能返回数组；旧的「取数列必须是单值」规则不能因为
        // 支持多选列而被悄悄放宽。
        let fields = std::collections::BTreeMap::from([("子".to_string(), json!(["A", "B"]))]);
        assert!(extract_values_owned(&snapshot_with(fields), "子", Some(20), None).is_empty());
    }

    #[test]
    fn option_row_replaces_the_whole_row_and_reenables() {
        // 整行替换：`enabled` 显式写 true，否则「停用→重新出现」的行永远复活不了
        let option = DerivedOption {
            option_id: "demo:abc".to_string(),
            label: "北京".to_string(),
            parent_key: String::new(),
            sort_order: 3,
        };
        let record = to_option_row("demo", &option);
        assert_eq!(record.get("option_id"), Some(&json!("demo:abc")));
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
        // `ancestors` 是借用切片，所以引用必须先算好再传进来（见 `as_raw` 的说明）
        let owned = OwnedRawValue {
            ancestors: vec!["大类".to_string(), "USD".to_string()],
            label: "6.9".to_string(),
        };
        let refs: Vec<&str> = owned.ancestors.iter().map(String::as_str).collect();
        let raw = owned.as_raw(&refs);
        assert_eq!(raw.label, "6.9");
        assert_eq!(raw.ancestors, vec!["大类", "USD"]);
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
            ancestors: &[],
            label: "CNY 人民币\n",
        }];
        let child_rows = [
            RawValue {
                ancestors: &["CNY 人民币\n"],
                label: "1.0000",
            },
            RawValue {
                ancestors: &["CNY 人民币"],
                label: "1.0000",
            },
        ];
        let parent = derive_options("currency", &[], &parent_rows);
        let child = derive_options("fx", &["currency"], &child_rows);
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
                ancestors: &["推广测评服务费"],
                label: "   ",
            },
            RawValue {
                ancestors: &["   "],
                label: "pay for services-X",
            },
        ];
        let derived = derive_options("summary", &["fee_type"], &rows);
        assert_eq!(derived.len(), 1, "空文案不产出选项（derive.rs 的 trim）");
        assert_eq!(derived[0].parent_key, "", "父文案为空时落无父键，不挂空父");
    }

    #[test]
    fn the_same_label_under_two_parents_yields_two_options() {
        // 目标表实测有 6 例「一子多父」（如 pay for services-YL-AR 同时属于
        // 推广测评服务费 与 预付储值款）。设计答案：各派生一个，互不覆盖。
        let rows = [
            RawValue {
                ancestors: &["推广测评服务费"],
                label: "pay for services-YL-AR",
            },
            RawValue {
                ancestors: &["预付储值款"],
                label: "pay for services-YL-AR",
            },
        ];
        let derived = derive_options("summary", &["fee_type"], &rows);
        assert_eq!(derived.len(), 2, "同文案不同父必须派生两个选项");
        assert_ne!(derived[0].parent_key, derived[1].parent_key);
    }

    fn field_item(field_id: &str, field_name: &str, field_type: i32) -> FieldItem {
        FieldItem {
            is_primary: false,
            field_id: field_id.to_string(),
            field_name: field_name.to_string(),
            field_type: Some(field_type),
            ui_type: None,
            property: None,
        }
    }

    fn bound_field<'a>(partition: &'a PartitionResult, field_id: &str) -> &'a BoundField {
        partition
            .bound
            .iter()
            .find(|field| field.field_id == field_id)
            .unwrap_or_else(|| panic!("{field_id} 应在可处理集合里"))
    }

    #[test]
    fn partition_resolves_current_names_and_keeps_parent_edges() {
        // 父列也要进快照：快照里靠**名字**取父列的值，拼不出父键的子树会整棵塌掉。
        // 名字取**本轮解析结果**，不是绑定行上的旧缓存。
        let bindings = vec![
            table_binding("fldA", Some("旧币种"), None),
            table_binding("fldB", Some("旧汇率"), Some("fldA")),
        ];
        let fields = vec![field_item("fldA", "币种", 3), field_item("fldB", "汇率", 2)];
        let partition = partition_bindings(&bindings, &fields);
        assert!(partition.skipped.is_empty());
        assert_eq!(partition.bound.len(), 2);
        assert_eq!(bound_field(&partition, "fldA").field_name, "币种");
        assert_eq!(
            bound_field(&partition, "fldB").parent_field_id.as_deref(),
            Some("fldA")
        );
    }

    #[test]
    fn a_missing_remote_field_skips_only_that_binding() {
        // 本次需求 D1 的核心：一个字段被删不再拖停整表。
        let bindings = vec![
            table_binding("fldA", Some("币种"), None),
            table_binding("fldGONE", Some("银行流水摘要"), None),
        ];
        let fields = vec![field_item("fldA", "币种", 3)];
        let partition = partition_bindings(&bindings, &fields);
        assert_eq!(partition.healthy_indices, vec![0]);
        assert_eq!(partition.skipped.len(), 1);
        assert_eq!(partition.skipped[0].field_id, "fldGONE");
        assert_eq!(
            partition.skipped[0].field_name.as_deref(),
            Some("银行流水摘要"),
            "远端字段已删，必须保留最后一次解析出来的名字给业务人员看"
        );
        assert_eq!(partition.skipped[0].reason, SkipReason::MissingField);
    }

    #[test]
    fn a_duplicate_field_name_skips_every_colliding_binding() {
        // 按名字取数会取到不确定的列；只跳过受影响的绑定，不拖停无关字段。
        let bindings = vec![
            table_binding("fldA", Some("币种"), None),
            table_binding("fldB", Some("币种"), None),
            table_binding("fldC", Some("汇率"), None),
        ];
        let fields = vec![
            field_item("fldA", "币种", 3),
            field_item("fldB", "币种", 3),
            field_item("fldC", "汇率", 2),
        ];
        let partition = partition_bindings(&bindings, &fields);
        assert_eq!(partition.healthy_indices, vec![2]);
        assert_eq!(partition.skipped.len(), 2);
        assert!(partition
            .skipped
            .iter()
            .all(|item| item.reason == SkipReason::DuplicateName));
    }

    #[test]
    fn an_unsupported_field_type_is_skipped() {
        // 人员/附件等类型的单元格里没有可用的选项文案；旧实现会静默产出空标签，
        // 再叠加补集停用会把整列选项清空。现在显式跳过并点名。
        let bindings = vec![table_binding("fldA", Some("附件列"), None)];
        let fields = vec![field_item("fldA", "附件列", 17)];
        let partition = partition_bindings(&bindings, &fields);
        assert!(partition.healthy_indices.is_empty());
        assert_eq!(partition.skipped[0].reason, SkipReason::UnsupportedType);
    }

    #[test]
    fn a_multi_select_leaf_is_healthy() {
        // 多选列按元素展开，可以作为叶子取数列。
        let bindings = vec![table_binding("fldA", Some("可提供的附件类型"), None)];
        let fields = vec![field_item("fldA", "可提供的附件类型", 4)];
        let partition = partition_bindings(&bindings, &fields);
        assert_eq!(partition.healthy_indices, vec![0]);
        assert!(partition.skipped.is_empty());
    }

    #[test]
    fn a_multi_select_parent_is_skipped_with_the_whole_subtree() {
        // 父键需要唯一文案；多选父会让子项挂到不确定的父上。
        // 父被跳过时，子树必须一起暂停（不能按无父处理）。
        let bindings = vec![
            table_binding("fldP", Some("多选父"), None),
            table_binding("fldC", Some("子"), Some("fldP")),
        ];
        let fields = vec![field_item("fldP", "多选父", 4), field_item("fldC", "子", 3)];
        let partition = partition_bindings(&bindings, &fields);
        assert!(partition.healthy_indices.is_empty());
        assert_eq!(partition.skipped[0].reason, SkipReason::MultiValueParent);
        assert_eq!(partition.skipped[1].reason, SkipReason::ParentSkipped);
    }

    #[test]
    fn a_missing_parent_skips_the_whole_subtree() {
        // 父列不在启用集合里 → 读不到父列文案，快照里也没有它 → 子树 option_id 整棵变。
        // 旧实现整表失败；现在只跳过这棵子树，并明确区分「父不可用」与「父也被跳过」。
        let bindings = vec![
            table_binding("fldA", Some("币种"), None),
            table_binding("fldB", Some("汇率"), Some("fldDisabled")),
            table_binding("fldC", Some("折算价"), Some("fldB")),
        ];
        let fields = vec![
            field_item("fldA", "币种", 3),
            field_item("fldB", "汇率", 2),
            field_item("fldC", "折算价", 2),
        ];
        let partition = partition_bindings(&bindings, &fields);
        assert_eq!(partition.healthy_indices, vec![0]);
        assert_eq!(partition.skipped.len(), 2);
        assert_eq!(partition.skipped[0].reason, SkipReason::ParentUnavailable);
        assert_eq!(
            partition.skipped[1].reason,
            SkipReason::ParentSkipped,
            "父被跳过时，子也必须一起暂停，不能按无父处理"
        );
    }

    #[test]
    fn a_parent_pointer_cycle_is_skipped_instead_of_hanging() {
        // `validate_fields` 拒自环与缺失父，但**手工改库能造出 a→b→a**。
        // 分区必须能收敛（不能死循环），并只跳过环上的绑定。
        let bindings = vec![
            table_binding("fldA", Some("甲"), Some("fldB")),
            table_binding("fldB", Some("乙"), Some("fldA")),
        ];
        let fields = vec![field_item("fldA", "甲", 3), field_item("fldB", "乙", 3)];
        let partition = partition_bindings(&bindings, &fields);
        assert!(partition.healthy_indices.is_empty());
        assert_eq!(partition.skipped.len(), 2);
        assert!(partition
            .skipped
            .iter()
            .all(|item| item.reason == SkipReason::ParentCycle));
    }

    #[test]
    fn parent_linkage_uses_the_resolved_name_not_the_cache() {
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
        let fields = vec![
            field_item("fldA", "币种（改名后）", 3),
            field_item("fldB", "汇率", 2),
        ];
        let partition = partition_bindings(&bindings, &fields);
        let linkage = parent_linkage(bound_field(&partition, "fldB"), &bindings, &partition.bound)
            .unwrap_or_else(|error| panic!("应可解析: {error}"))
            .unwrap_or_else(|| panic!("fldB 声明了父列"));
        assert_eq!(
            linkage.ancestors,
            vec![LinkageLevel {
                source_key: "currency".to_string(),
                field_name: "币种（改名后）".to_string(),
            }]
        );
    }

    #[test]
    fn the_ancestor_chain_is_built_root_first_for_any_depth() {
        // 上溯得到的是「直接父 → 根」，而折叠必须从根往下——顺序反了会让父键整条算错，
        // 且错法是「静默空下拉」，不报错。真机上那条链是四级。
        let bindings = vec![
            table_binding("fldL1", Some("交易类型"), None),
            table_binding("fldL2", Some("费用大类"), Some("fldL1")),
            table_binding("fldL3", Some("费用类型"), Some("fldL2")),
            table_binding("fldL4", Some("银行流水摘要"), Some("fldL3")),
        ];
        let fields = vec![
            field_item("fldL1", "交易类型", 3),
            field_item("fldL2", "费用大类", 3),
            field_item("fldL3", "费用类型", 3),
            field_item("fldL4", "银行流水摘要", 3),
        ];
        let partition = partition_bindings(&bindings, &fields);
        assert!(partition.skipped.is_empty());
        let linkage = parent_linkage(
            bound_field(&partition, "fldL4"),
            &bindings,
            &partition.bound,
        )
        .unwrap_or_else(|error| panic!("应可解析: {error}"))
        .unwrap_or_else(|| panic!("fldL4 声明了父列"));
        assert_eq!(
            linkage
                .ancestors
                .iter()
                .map(|level| level.field_name.as_str())
                .collect::<Vec<_>>(),
            vec!["交易类型", "费用大类", "费用类型"],
            "从根到直接父；反了的话折叠出来是另一条键"
        );
        assert_eq!(linkage.source_keys().len(), 3);
    }

    #[test]
    fn parent_linkage_still_guards_cycles_when_called_directly() {
        // 分区已经会跳过成环绑定；但 `parent_linkage` 还有第二个消费者
        // （xlsx 导入），守卫不能因为分区存在就拆掉。
        let bindings = vec![
            table_binding("fldA", Some("甲"), Some("fldB")),
            table_binding("fldB", Some("乙"), Some("fldA")),
        ];
        let bound = vec![
            BoundField {
                field_id: "fldA".to_string(),
                field_name: "甲".to_string(),
                parent_field_id: Some("fldB".to_string()),
            },
            BoundField {
                field_id: "fldB".to_string(),
                field_name: "乙".to_string(),
                parent_field_id: Some("fldA".to_string()),
            },
        ];
        let error = parent_linkage(&bound[0], &bindings, &bound)
            .err()
            .unwrap_or_else(|| panic!("成环必须失败，不能静默上溯"));
        assert!(
            error.to_string().contains("成环"),
            "错误里要说明是成环: {error}"
        );
    }

    #[test]
    fn a_binding_without_a_parent_has_no_linkage() {
        let bindings = vec![table_binding("fldA", Some("币种"), None)];
        let fields = vec![field_item("fldA", "币种", 3)];
        let partition = partition_bindings(&bindings, &fields);
        let linkage = parent_linkage(bound_field(&partition, "fldA"), &bindings, &partition.bound)
            .unwrap_or_else(|error| panic!("应可解析: {error}"));
        assert!(linkage.is_none());
    }

    #[test]
    fn a_blank_parent_pointer_is_treated_as_no_parent() {
        // 界面清空父列时可能提交空串而不是 null；当成「没有父」，而不是拿一个空 id
        // 去解析（那会让整表因为一个不存在的字段停摆）。
        let bindings = vec![table_binding("fldA", Some("币种"), Some("   "))];
        let fields = vec![field_item("fldA", "币种", 3)];
        let partition = partition_bindings(&bindings, &fields);
        assert!(partition.skipped.is_empty());
        assert!(bound_field(&partition, "fldA").parent_field_id.is_none());
    }

    #[test]
    fn the_skip_summary_names_the_field_for_business_users() {
        let skipped = vec![SkippedBinding {
            source_key: "old_rate".to_string(),
            field_id: "fldGONE".to_string(),
            field_name: Some("银行流水摘要".to_string()),
            reason: SkipReason::MissingField,
        }];
        let summary = skipped_summary(&skipped);
        assert!(
            summary.contains("银行流水摘要"),
            "必须点名字段名: {summary}"
        );
        assert!(
            summary.contains("fldGONE"),
            "ID 作为次要定位信息: {summary}"
        );
        assert!(
            summary.contains("old_rate"),
            "source_key 作为次要定位信息: {summary}"
        );
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
