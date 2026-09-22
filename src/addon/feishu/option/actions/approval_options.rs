//! 飞书审批「关联外部选项」取数端点。
//!
//! 这是本服务对外暴露的**唯一**字面严格接口：响应是 [`ResponseBody::raw`] 产出的
//! `{code,msg,data}`，不套框架的 `{code,message,data}` 包络。
//!
//! # 三条不可动摇的约定
//!
//! 1. **任何业务失败都返回 `Ok(ResponseBody::raw(失败信封))`，不用 `Err`。**
//!    匿名端点的 `Err` 会在可观测性里记成 `result="error"` 并消耗全站可用性预算，
//!    而 burn-rate 规则按 `sum by (job)` 聚合、不带 operation 维度——飞书的探活
//!    流量不该把全站告警打起来。
//! 2. **HTTP 状态恒 200**，成败由信封里的 `code` 承载。非 200 会让飞书按「接口报错」
//!    处理，而官方 FAQ 对失败现象的归因正是「外部数据源返回接口报错，所以获取选项失败」。
//! 3. **2.5 秒主动收口**。飞书对这次回调的超时是 3 秒；主动收口能返回一个可归因的
//!    失败信封，而不是被掐断、留下既无响应也无法诊断的黑洞。

use std::sync::Arc;

use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::table::{SortOrder, WhereCondition};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::i18n::{
    build_result_body, normalize_linkage_value, OptionRow, DEFAULT_LOCALE,
};
use crate::addon::feishu::domain::linkage::{match_linkage, parse_linkage_mapping};
use crate::addon::feishu::domain::pagination::{decode_cursor, encode_cursor};
use crate::addon::feishu::domain::protocol::{FeishuEnvelope, FeishuOptionsRequest};
use crate::addon::feishu::domain::token::verify_token;

/// 单次请求返回的最大选项数。
///
/// 框架 `TableQuery` 的硬上限是 100（`MAX_QUERY_PAGE_SIZE`），飞书要求 page size
/// 不小于 10——100 同时满足两者。
///
/// **这个值恰好等于框架硬上限，所以任何「多取一行」的写法都会越界。** 判定 hasMore
/// 只能用 `PaginatedResult::total`（见 `resolve` 与 `next_page_token` 的说明）。
const PAGE_SIZE: usize = 100;

/// 编译期守护：`PAGE_SIZE` 超过框架硬上限会让 `TableQuery::page` **拒绝**（不是 clamp），
/// 端点随即对每一个合法请求恒返回 `code=50001`。
///
/// 这不是假想的风险——常量恰好等于上限时，任何「多取一行」的写法都必然越界，而这个
/// 缺陷曾经上线过（110 条单测全绿、端点全挂）。把这类越界从「飞书联调时才发现」提前到
/// 「构建期就炸」。
const _: () = assert!(PAGE_SIZE <= yang_base::table::MAX_TABLE_QUERY_PAGE_SIZE);

/// 内部处理预算；飞书的回调超时是 3 秒。
const PROCESSING_BUDGET: std::time::Duration = std::time::Duration::from_millis(2_500);

/// 失败信封的错误码。
///
/// 飞书只依据 `code` 是否为 0 判定成败，具体数值供我们自己在日志里归因。
mod codes {
    /// 数据源不存在。
    pub(super) const SOURCE_NOT_FOUND: i32 = 40401;
    /// 请求未带 token。
    pub(super) const TOKEN_MISSING: i32 = 40101;
    /// token 不匹配。
    pub(super) const TOKEN_MISMATCH: i32 = 40102;
    /// 数据源已停用。
    pub(super) const SOURCE_DISABLED: i32 = 40301;
    /// 分页标记非法。
    pub(super) const CURSOR_INVALID: i32 = 40002;
    /// 数据源启用了加密但服务端没配密钥。
    pub(super) const ENCRYPTION_NOT_CONFIGURED: i32 = 50002;
    /// 服务内部错误。
    pub(super) const INTERNAL: i32 = 50001;
    /// 服务处理超时。
    pub(super) const TIMEOUT: i32 = 50401;
    /// 联动参数命中了多个映射，无法判定用哪一个父级。
    pub(super) const LINKAGE_AMBIGUOUS: i32 = 40003;
    /// 联动参数归一化后无法解析成已知的父选项。
    pub(super) const LINKAGE_NOT_RESOLVED: i32 = 40004;
}

/// 读端的级联过滤决策。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentFilter {
    /// 不做父级过滤，回退全量。
    All,
    /// 只返回该父键下的选项。
    ByKey(String),
}

/// **纯决策**：从请求与映射里算出「要按父数据源下的哪个值过滤」。
///
/// 返回 `Ok(None)` 表示**回退全量**——这是默认，只在能确定唯一父级时才收窄。
/// 四条回退分支都是刻意选的：
///
/// 1. 不带 `linkage_params` → 回退全量。契约 C3 的硬要求，也是「一个控件配一个
///    `source_key`」的原因。
/// 2. 映射缺失或解析不出 → 回退全量。返回空集是最难归因的失败形态。
/// 3. 命中 0 个映射键 → 回退全量；**命中 ≥2 个才 fail-closed**——能确定唯一父级
///    时不该因为请求里多带了一个无关键就报错。
/// 4. 值为空或 trim 后为空 → 回退全量。用户尚未选父级是最常见的形态，
///    此时返回空集会让控件看起来「坏了」。
///
/// 与 [`resolve_parent_key`] 分开是为了可测：这四条分支最容易在改动中被无声破坏——
/// 它们的失效形态是「选项集静默变大或变空」，不会报错。
fn linkage_filter_target(
    input: &FeishuOptionsRequest,
    linkage_mapping: Option<&str>,
) -> Result<Option<(String, String)>, FeishuEnvelope> {
    // 1) 不带联动参数 → 回退全量。契约 C3 的硬要求，也是「一个控件配一个 source_key」的原因。
    let Some(params) = input.linkage_params.as_ref().filter(|map| !map.is_empty()) else {
        return Ok(None);
    };
    // 2) 映射缺失或解析不出 → 回退全量。返回空集是最难归因的失败形态。
    let Some(raw) = linkage_mapping.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    // 3) 挑唯一命中的那条声明。精确键优先于通配键，规则见 `domain/linkage.rs`。
    let entries = parse_linkage_mapping(raw);
    let (matched_key, linkage) = match match_linkage(&entries, params.keys().map(String::as_str)) {
        Ok(Some(found)) => found,
        // 命中 0 条 → 回退全量：请求里多带一个无关键不该让请求失败。
        Ok(None) => return Ok(None),
        // 命中 ≥2 条 → 无法判定父级，fail-closed。
        Err(()) => {
            return Err(FeishuEnvelope::fail(
                codes::LINKAGE_AMBIGUOUS,
                "联动参数命中了多个映射，无法判定父级",
            ))
        }
    };

    // 4) 值为空或 trim 后为空 → 回退全量。用户尚未选父级是最常见的形态，
    //    此时返回空集会让控件看起来「坏了」。
    let Some(raw_value) = params.get(matched_key) else {
        return Ok(None);
    };
    let parent_key = normalize_linkage_value(raw_value);
    if parent_key.is_empty() {
        tracing::warn!("联动参数为空，本次回退全量");
        return Ok(None);
    }

    Ok(Some((
        linkage.parent_source_key.clone(),
        parent_key.to_string(),
    )))
}

/// 决定本次请求要不要按父级过滤，并在需要时确认父值真实存在。
///
/// 四条回退分支见 [`linkage_filter_target`]；这里是第 5 条：**归一化后匹配不上要
/// fail-closed 返回可归因业务码，绝不静默返回 0 行**——「这个父值没有子项」与
/// 「父值根本不存在」是两件事，控制台要看得出区别。
///
/// 返回值的两层是刻意分开的：外层 `BaseError` 是**内部故障**（数据库不可用等），
/// 内层 `FeishuEnvelope` 是**可归因的业务失败**。两者对飞书的意义完全不同——
/// 前者值得重试，后者重试也没用。
async fn resolve_parent_key(
    input: &FeishuOptionsRequest,
    linkage_mapping: Option<&str>,
    context: &FeishuContext,
) -> Result<Result<ParentFilter, FeishuEnvelope>, BaseError> {
    let (parent_source_key, parent_key) = match linkage_filter_target(input, linkage_mapping) {
        Ok(Some(target)) => target,
        Ok(None) => return Ok(Ok(ParentFilter::All)),
        Err(envelope) => return Ok(Err(envelope)),
    };

    // 查一次确认父值存在，而不是直接拿去过滤：直接过滤的结果是「命中 0 行」，而它与
    // 「这个父值确实没有子项」无法区分——前者是数据/契约问题，后者是正常的空集，
    // 控制台必须能分辨。
    let exists = context
        .options()
        .query()
        .select_fields(&["option_id"])?
        .where_eq(
            "source_key",
            serde_json::Value::String(parent_source_key.clone()),
        )?
        .where_eq("option_id", serde_json::Value::String(parent_key.clone()))?
        .optional()
        .await?
        .is_some();
    if !exists {
        // 按 (source_key, label) 再查一次的兜底**没有实现**：`feishu_option.label`
        // 不是 `filterable` 列（DSL 的 filterable 是 fail-closed），按它过滤会在运行期
        // 吃 FieldPermissionDenied。而这条路径在契约下本就不该被执行——飞书回传的是
        // 我们给它的 `value`，剥掉 `@i18n@` 就是 option_id。真走到这里说明契约变了，
        // 应当是可归因的失败而不是一次模糊匹配。
        return Ok(Err(FeishuEnvelope::fail(
            codes::LINKAGE_NOT_RESOLVED,
            format!("联动值无法解析为数据源 {parent_source_key} 下的已知选项"),
        )));
    }
    Ok(Ok(ParentFilter::ByKey(parent_key)))
}

/// 校验数据源与 Token；失败时给出信封。/// 校验数据源与 Token；失败时给出信封。
///
/// 抽成纯函数以便不依赖数据库做单元测试——这是本端点唯一能在单测里覆盖的判定逻辑。
fn verify_source(
    stored_hash: Option<&str>,
    presented: Option<&str>,
    is_active: bool,
) -> Result<(), FeishuEnvelope> {
    let stored =
        stored_hash.ok_or_else(|| FeishuEnvelope::fail(codes::SOURCE_NOT_FOUND, "数据源不存在"))?;
    let presented =
        presented.ok_or_else(|| FeishuEnvelope::fail(codes::TOKEN_MISSING, "缺少 token"))?;
    if !verify_token(presented, stored) {
        return Err(FeishuEnvelope::fail(
            codes::TOKEN_MISMATCH,
            "token 校验失败",
        ));
    }
    if !is_active {
        return Err(FeishuEnvelope::fail(codes::SOURCE_DISABLED, "数据源已停用"));
    }
    Ok(())
}

/// 注册取选项端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("approval_options"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/options/{source_key}",
        )
        .display_name("飞书外部选项")
        .description("飞书审批单选/多选控件的外部选项数据源接口")
        // public：飞书带自定义 Token 直连，不经框架 JWT 鉴权；来源校验在 handler 内完成
        .public()
        .register()
}

/// 处理一次取选项请求。
pub(super) async fn handle(
    ctx: ActionContext,
    input: FeishuOptionsRequest,
    context: Arc<FeishuContext>,
) -> Result<ResponseBody, BaseError> {
    let envelope =
        match tokio::time::timeout(PROCESSING_BUDGET, resolve(&ctx, &input, &context)).await {
            Ok(Ok(envelope)) => envelope,
            Ok(Err(error)) => {
                // 内部故障也返回 200 + 失败信封：非 200 会让飞书按「接口报错」处理
                tracing::error!(error = %error, code = error.code(), "飞书取选项请求失败");
                FeishuEnvelope::fail(codes::INTERNAL, "服务内部错误")
            }
            Err(_elapsed) => {
                // 主动收口而不是被飞书在 3 秒处掐断
                tracing::error!("飞书取选项请求超出内部处理预算");
                FeishuEnvelope::fail(codes::TIMEOUT, "服务处理超时")
            }
        };
    let body = envelope.to_json()?;
    ResponseBody::raw(body, "application/json")
}

/// 解析并组装成功响应；业务失败以 `Ok(失败信封)` 返回。
async fn resolve(
    ctx: &ActionContext,
    input: &FeishuOptionsRequest,
    context: &FeishuContext,
) -> Result<FeishuEnvelope, BaseError> {
    let source_key = ctx
        .request
        .get_path_param("source_key")
        .ok_or_else(|| {
            BaseError::ParamInvalid("source_key".to_string(), "缺少数据源标识".to_string())
        })?
        .to_string();

    let datasource = context
        .datasources()
        .query()
        .select_fields(&[
            "source_key",
            "token_hash",
            "encrypt_enabled",
            "default_locale",
            "status",
            // 级联映射：读端要按它决定「这个请求的联动参数对应哪个父数据源」。
            "linkage_mapping",
        ])?
        .where_eq("source_key", serde_json::Value::String(source_key.clone()))?
        .optional()
        .await?;

    let Some(datasource) = datasource else {
        return Ok(FeishuEnvelope::fail(
            codes::SOURCE_NOT_FOUND,
            "数据源不存在",
        ));
    };

    let stored_hash: String = datasource.require("token_hash")?;
    let status: String = datasource.require("status")?;
    if let Err(envelope) = verify_source(Some(&stored_hash), Some(&input.token), status == "active")
    {
        return Ok(envelope);
    }

    let encrypt_enabled: bool = datasource.optional("encrypt_enabled")?.unwrap_or(false);
    let default_locale: String = datasource
        .optional("default_locale")?
        .unwrap_or_else(|| DEFAULT_LOCALE.to_string());

    // 非法游标 fail-closed：静默从头返回会让翻页重复吐第一页
    let cursor = match input.page_token.as_deref() {
        Some(raw) => match decode_cursor(raw) {
            Ok(value) => value,
            Err(_) => return Ok(FeishuEnvelope::fail(codes::CURSOR_INVALID, "分页标记非法")),
        },
        None => None,
    };

    let mut query = context
        .options()
        .query()
        .select_fields(&["option_id", "label", "i18n", "sort_order", "is_default"])?
        .where_eq("source_key", serde_json::Value::String(source_key.clone()))?
        .where_eq("enabled", serde_json::Value::Bool(true))?
        .search(input.query.as_deref())?
        .order_by("sort_order", SortOrder::Asc)?
        .order_by("option_id", SortOrder::Asc)?;

    // 级联过滤：**挂在顶层**（顶层条件之间是 AND，不影响 keyset 游标）。
    // 必须在游标解码之后、构造查询之前，否则游标条件会与它争同一层。
    let linkage_raw = datasource.optional::<String>("linkage_mapping")?;
    match resolve_parent_key(input, linkage_raw.as_deref(), context).await? {
        Ok(ParentFilter::All) => {}
        Ok(ParentFilter::ByKey(parent_key)) => {
            query = query.where_eq("parent_key", serde_json::Value::String(parent_key))?;
        }
        Err(envelope) => return Ok(envelope),
    }

    if let Some((sort_order, option_id)) = cursor {
        // keyset 翻页：排序键严格大于游标。用 offset 会在数据变动时漏行或重复，
        // 而审批人翻页时数据被多维表格改掉是常态。
        query = query.where_or(vec![
            WhereCondition::Gt {
                field: "sort_order".to_string(),
                value: serde_json::Value::Number(sort_order.into()),
            },
            WhereCondition::And {
                conditions: vec![
                    WhereCondition::Eq {
                        field: "sort_order".to_string(),
                        value: serde_json::Value::Number(sort_order.into()),
                    },
                    WhereCondition::Gt {
                        field: "option_id".to_string(),
                        value: serde_json::Value::String(option_id),
                    },
                ],
            },
        ])?;
    }

    // 一页取满 PAGE_SIZE 条。**不能用「多取一行」判定 hasMore**：`TableQuery::page`
    // 对页大小是**拒绝**而非 clamp，`page(1, PAGE_SIZE + 1)` 会直接返回
    // `ParamInvalid`，把整个端点打成恒失败的 `code=50001`（`page_size` 硬上限恰好
    // 就是 100，多取一行必然越界）。
    //
    // `paginate_records` 本来就会执行一次 COUNT(*)（`read.rs` 的 `paginate`），
    // 所以改为借它的 `total` 判定下一页：既不额外取行，也不额外查询。
    let page = query.page(1, PAGE_SIZE)?.paginate_records().await?;
    let total = page.total;

    let mut rows = Vec::with_capacity(page.data.len());
    for record in page.data {
        rows.push(read_row(&record)?);
    }

    // `next_page_token` 是 hasMore 的**唯一来源**，见函数文档。
    let next_page_token = next_page_token(&rows, total);
    let has_more = next_page_token.is_some();

    let result = build_result_body(&rows, &default_locale, has_more, next_page_token);

    if !encrypt_enabled {
        return Ok(FeishuEnvelope::ok(result));
    }
    let Some(key) = context.encryption_key() else {
        // 数据源开了加密但服务端没配密钥：这是可诊断的配置事故，按业务失败回给飞书
        // （信封里带上原因比一个裸 500 好排查），同时记 error 让运维看到
        tracing::error!(source_key = %source_key, "数据源启用了加密但未配置 feishu.encryption_key");
        return Ok(FeishuEnvelope::fail(
            codes::ENCRYPTION_NOT_CONFIGURED,
            "服务端未配置加密密钥",
        ));
    };
    let value = serde_json::to_value(&result)
        .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?;
    Ok(FeishuEnvelope::encrypted(
        crate::addon::feishu::domain::crypto::encrypt_json(&value, &key)?,
    ))
}

/// 由本页结果推导下一页游标。**它的 `Option` 同时就是 `hasMore`**。
///
/// # 为什么 hasMore 必须从这里反推
///
/// 飞书文档对这两个字段的关系是硬约束：**`hasMore` 为 true 时「会同时返回新的
/// `nextPageToken`」，否则不返回 `nextPageToken`**。而 `total`（COUNT）与 `rows`
/// （SELECT）是 `paginate` 里**两条独立的 autocommit 语句**（查询绑在连接池上、不在
/// 事务里），并发写会让两者看到不同快照：`delete_options` 单批就能停用 500 条，而本
/// 端点恰好按 `enabled = true` 过滤——「COUNT 时还有、SELECT 时已被停用」完全可达。
///
/// 若把 `has_more` 直接写成 `total > rows.len()`，那种情形下会得到「空 `options` +
/// `hasMore: true` + 没有 `nextPageToken`」——一个文档明令禁止的组合，而游标本来就
/// 只能由本页最后一行编码，空页根本给不出。把 `has_more` 定义成「游标存在」，这个
/// 组合在结构上就不可能再出现。
///
/// # 契约
///
/// | 本页行数 | `total` | 结果 | 说明 |
/// |---|---|---|---|
/// | 100 | 105 | `Some` | 取满且 COUNT 说还有剩余 |
/// | 100 | 100 | `None` | 恰好取满，确实没有下一页 |
/// | 50 | 50 | `None` | 不足一页，已到末尾 |
/// | 0 | 0 | `None` | 空结果 |
/// | 0 | 105 | `None` | COUNT/SELECT 不一致（并发停用清空了本页）——以 SELECT 为准 |
fn next_page_token(rows: &[OptionRow], total: usize) -> Option<String> {
    if total <= rows.len() {
        return None;
    }
    // 游标必须用该行真实的 `sort_order`，否则下一页的 keyset 条件会错位
    rows.last()
        .map(|last| encode_cursor(last.sort_order, &last.option_id))
}

/// 把一行记录读成响应所需的形状。
///
/// `i18n` 落的是 Text 列存放 JSON 文本（DSL 没有 Json builder）——空值与空串都视为
/// 「没有额外语言」，而不是解析失败。
fn read_row(record: &yang_base::table::Record) -> Result<OptionRow, BaseError> {
    let i18n_text: Option<String> = record.optional("i18n")?;
    let i18n = match i18n_text.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => serde_json::from_str(raw)
            .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))?,
        _ => std::collections::BTreeMap::new(),
    };
    Ok(OptionRow {
        option_id: record.require("option_id")?,
        label: record.require("label")?,
        i18n,
        sort_order: record.require("sort_order")?,
        is_default: record.optional("is_default")?.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_for(token: &str) -> String {
        crate::addon::feishu::domain::token::hash_token(token)
    }

    #[test]
    fn matching_token_on_active_source_passes() {
        let stored = stored_for("right-token");
        assert!(verify_source(Some(&stored), Some("right-token"), true).is_ok());
    }

    #[test]
    fn wrong_token_yields_failure_envelope_not_error() {
        // Token 不匹配必须给出业务失败信封（HTTP 200、code != 0），不能是 Err
        let stored = stored_for("right-token");
        let envelope = verify_source(Some(&stored), Some("wrong-token"), true)
            .err()
            .unwrap_or_else(|| panic!("不匹配应给出失败信封"));
        assert_eq!(envelope.code, codes::TOKEN_MISMATCH);
        assert!(!envelope.msg.is_empty(), "msg 应说明原因");
        assert!(envelope.data.is_none(), "失败信封不应带 data");
    }

    #[test]
    fn missing_source_yields_not_found_envelope() {
        let envelope = verify_source(None, Some("any"), true)
            .err()
            .unwrap_or_else(|| panic!("数据源不存在应给出失败信封"));
        assert_eq!(envelope.code, codes::SOURCE_NOT_FOUND);
    }

    #[test]
    fn missing_token_is_distinguishable_from_wrong_token() {
        // 两者错误码不同，便于运维从日志区分「没配 Token」与「Token 配错了」
        let stored = stored_for("right-token");
        let missing = verify_source(Some(&stored), None, true)
            .err()
            .unwrap_or_else(|| panic!("缺 token 应给出失败信封"));
        assert_eq!(missing.code, codes::TOKEN_MISSING);
        assert_ne!(missing.code, codes::TOKEN_MISMATCH);
    }

    #[test]
    fn disabled_source_yields_failure_envelope() {
        let stored = stored_for("right-token");
        let envelope = verify_source(Some(&stored), Some("right-token"), false)
            .err()
            .unwrap_or_else(|| panic!("停用数据源应给出失败信封"));
        assert_eq!(envelope.code, codes::SOURCE_DISABLED);
    }

    #[test]
    fn token_is_checked_before_status() {
        // 顺序很重要：错误 Token 不该探知数据源是否停用
        let stored = stored_for("right-token");
        let envelope = verify_source(Some(&stored), Some("wrong-token"), false)
            .err()
            .unwrap_or_else(|| panic!("应给出失败信封"));
        assert_eq!(
            envelope.code,
            codes::TOKEN_MISMATCH,
            "凭证错误应先于状态检查被报出，避免泄漏数据源状态"
        );
    }

    #[test]
    fn failure_codes_are_nonzero_and_distinct() {
        let all = [
            codes::SOURCE_NOT_FOUND,
            codes::TOKEN_MISSING,
            codes::TOKEN_MISMATCH,
            codes::SOURCE_DISABLED,
            codes::CURSOR_INVALID,
            codes::ENCRYPTION_NOT_CONFIGURED,
            codes::INTERNAL,
            codes::TIMEOUT,
        ];
        for code in all {
            assert_ne!(code, 0, "失败码不得为 0（0 表示成功）");
        }
        let unique: std::collections::BTreeSet<i32> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "失败码必须两两不同");
    }

    #[test]
    fn read_row_defaults_missing_i18n_to_empty_map() {
        let mut record = yang_base::table::Record::new();
        record.insert("option_id", serde_json::json!("a"));
        record.insert("label", serde_json::json!("甲"));
        record.insert("sort_order", serde_json::json!(3));
        let row = read_row(&record).unwrap_or_else(|error| panic!("应可读取: {error}"));
        assert!(row.i18n.is_empty(), "缺 i18n 列应为空 map");
        assert_eq!(row.option_id, "a");
        assert_eq!(row.sort_order, 3);
        assert!(!row.is_default, "缺 is_default 列应为 false");
    }

    #[test]
    fn read_row_treats_blank_i18n_as_empty() {
        let mut record = yang_base::table::Record::new();
        record.insert("option_id", serde_json::json!("a"));
        record.insert("label", serde_json::json!("甲"));
        record.insert("sort_order", serde_json::json!(0));
        record.insert("i18n", serde_json::json!("   "));
        let row = read_row(&record).unwrap_or_else(|error| panic!("应可读取: {error}"));
        assert!(row.i18n.is_empty(), "空白 i18n 应视为没有额外语言");
    }

    #[test]
    fn read_row_parses_i18n_json_text() {
        let mut record = yang_base::table::Record::new();
        record.insert("option_id", serde_json::json!("a"));
        record.insert("label", serde_json::json!("甲"));
        record.insert("sort_order", serde_json::json!(0));
        record.insert("i18n", serde_json::json!(r#"{"en_us":"Alpha"}"#));
        record.insert("is_default", serde_json::json!(true));
        let row = read_row(&record).unwrap_or_else(|error| panic!("应可读取: {error}"));
        assert_eq!(row.i18n.get("en_us").map(String::as_str), Some("Alpha"));
        assert!(row.is_default);
    }

    /// 造 `count` 行选项，`sort_order` 从 `start` 起递增。
    fn rows(count: usize, start: i64) -> Vec<OptionRow> {
        (0..count)
            .map(|index| OptionRow {
                option_id: format!("opt_{:03}", start + index as i64),
                label: format!("选项{index}"),
                i18n: std::collections::BTreeMap::new(),
                sort_order: start + index as i64,
                is_default: false,
            })
            .collect()
    }

    #[test]
    fn next_page_token_contract_is_the_whole_has_more_decision() {
        // 取满整页且 COUNT 说还有剩余 -> 给出游标（hasMore 为真）
        assert!(next_page_token(&rows(100, 0), 105).is_some(), "应给出游标");
        // 恰好取满、COUNT 说没有了 -> 不给游标（等价于 hasMore=false）
        assert!(
            next_page_token(&rows(100, 0), 100).is_none(),
            "不得给出游标"
        );
        // 不足一页 -> 已到末尾
        assert!(next_page_token(&rows(50, 0), 50).is_none(), "不得给出游标");
        // 空结果
        assert!(next_page_token(&[], 0).is_none(), "不得给出游标");
    }

    #[test]
    fn next_page_token_is_absent_when_a_concurrent_disable_emptied_the_page() {
        // COUNT 与 SELECT 是 `paginate` 里两条独立的 autocommit 语句，并发停用可以让
        // 「COUNT 时还有」的行在 SELECT 前消失。此时若仍按 `total > rows.len()` 判定，
        // 响应会出现 `hasMore: true` 配**空 options**、且完全没有 `nextPageToken`——
        // 而文档明文要求 hasMore 为 true 时必须同时返回 nextPageToken，且游标只能由本页
        // 最后一行编码，空页根本给不出。以 SELECT 为准即不可能出现该组合。
        assert!(
            next_page_token(&[], 105).is_none(),
            "空页给不出游标，就不能声称还有下一页"
        );
    }

    #[test]
    fn next_page_token_encodes_the_last_row_of_the_page() {
        // 游标必须落在**已交付**的最后一行上，否则下一页的 keyset 条件会错位
        let token = next_page_token(&rows(100, 0), 105).unwrap_or_else(|| panic!("应给出游标"));
        assert_eq!(
            decode_cursor(&token).unwrap_or_else(|error| panic!("游标应可解码: {error}")),
            Some((99, "opt_099".to_string()))
        );
    }

    #[test]
    fn read_row_rejects_malformed_i18n_json() {
        // 坏数据应显式失败，而不是静默丢文案——静默丢会让控件显示为空且无从归因
        let mut record = yang_base::table::Record::new();
        record.insert("option_id", serde_json::json!("a"));
        record.insert("label", serde_json::json!("甲"));
        record.insert("sort_order", serde_json::json!(0));
        record.insert("i18n", serde_json::json!("{not json"));
        assert!(read_row(&record).is_err(), "非法 JSON 文本必须报错");
    }

    // ---- 级联过滤的纯决策（四条回退分支 + 一条失败分支）----

    /// 造一个请求；`linkage_params` 为 `None` 表示不带联动。
    fn request_with(linkage: Option<&[(&str, &str)]>) -> FeishuOptionsRequest {
        let mut body = serde_json::json!({"token": "t0ken"});
        if let Some(pairs) = linkage {
            let map: serde_json::Map<String, serde_json::Value> = pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), serde_json::json!(value)))
                .collect();
            body["linkage_params"] = serde_json::Value::Object(map);
        }
        let mut request = yang_base::action::Request::new(body);
        <FeishuOptionsRequest as yang_base::definition::ParamInput>::decode(&mut request)
            .unwrap_or_else(|error| panic!("测试请求应可解码: {error}"))
    }

    const MAPPING: &str = r#"{"widget1":{"parent_source_key":"payment_currency",
        "parent_field":"币种","cascade_field":"汇率"}}"#;

    #[test]
    fn no_linkage_params_falls_back_to_all() {
        // 契约 C3 的硬要求：不带联动必须返回全量
        let target = linkage_filter_target(&request_with(None), Some(MAPPING))
            .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(target, None);
    }

    #[test]
    fn empty_linkage_params_falls_back_to_all() {
        let target = linkage_filter_target(&request_with(Some(&[])), Some(MAPPING))
            .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(target, None);
    }

    #[test]
    fn missing_or_broken_mapping_falls_back_to_all() {
        // 空集是最难归因的失败形态，宁可回退全量
        for mapping in [None, Some(""), Some("   "), Some("{not json"), Some("{}")] {
            let target = linkage_filter_target(&request_with(Some(&[("widget1", "x")])), mapping)
                .unwrap_or_else(|_| panic!("mapping={mapping:?} 不该产生业务失败"));
            assert_eq!(target, None, "mapping={mapping:?} 应回退全量");
        }
    }

    #[test]
    fn an_unmatched_widget_key_falls_back_to_all() {
        // 请求里带的键不在映射里 → 回退全量，而不是报错
        let target = linkage_filter_target(&request_with(Some(&[("other", "x")])), Some(MAPPING))
            .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(target, None);
    }

    #[test]
    fn blank_value_falls_back_to_all() {
        // 用户尚未选父级是最常见形态，返回空集会让控件看起来「坏了」
        for value in ["", "   ", "@i18n@", "@i18n@  "] {
            let target =
                linkage_filter_target(&request_with(Some(&[("widget1", value)])), Some(MAPPING))
                    .unwrap_or_else(|_| panic!("value={value:?} 不该产生业务失败"));
            assert_eq!(target, None, "value={value:?} 应回退全量");
        }
    }

    #[test]
    fn a_single_matched_key_yields_the_parent_target() {
        let target = linkage_filter_target(
            &request_with(Some(&[("widget1", "@i18n@payment_currency:abc123")])),
            Some(MAPPING),
        )
        .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(
            target,
            Some((
                "payment_currency".to_string(),
                "payment_currency:abc123".to_string()
            )),
            "剥掉 @i18n@ 后剩下的就是父的 option_id"
        );
    }

    #[test]
    fn a_bare_id_without_prefix_is_accepted_too() {
        let target = linkage_filter_target(
            &request_with(Some(&[("widget1", "payment_currency:abc123")])),
            Some(MAPPING),
        )
        .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(
            target,
            Some((
                "payment_currency".to_string(),
                "payment_currency:abc123".to_string()
            ))
        );
    }

    #[test]
    fn multiple_matched_keys_fail_closed() {
        // 命中 ≥2 个参数时无法判定父级——这是唯一在决策期就失败的形态。
        // 映射本身必须**完整**（两个成员都在），否则会被解析器按「配错的条目」跳过，
        // 于是「两个参数」根本不会被判成歧义。
        let mapping = r#"{"widget1":{"parent_source_key":"a","parent_field":"p"},
                          "widget2":{"parent_source_key":"b","parent_field":"p"}}"#;
        let error = linkage_filter_target(
            &request_with(Some(&[("widget1", "x"), ("widget2", "y")])),
            Some(mapping),
        )
        .err()
        .unwrap_or_else(|| panic!("命中多个映射必须失败"));
        assert_eq!(error.code, codes::LINKAGE_AMBIGUOUS);
    }

    #[test]
    fn a_wildcard_key_matches_any_linkage_param() {
        // 通配的目的：不必知道飞书那个控件的字段代码。一个数据源服务一个控件，
        // 所以声明了级联时出现的任何联动参数只可能是它。
        let mapping = r#"{"*":{"parent_source_key":"payment_currency","parent_field":"币种"}}"#;
        for key in ["widget17796881173030001", "随便什么键"] {
            let target = linkage_filter_target(
                &request_with(Some(&[(key, "@i18n@payment_currency:abc")])),
                Some(mapping),
            )
            .unwrap_or_else(|_| panic!("通配不该产生业务失败"));
            assert_eq!(
                target,
                Some((
                    "payment_currency".to_string(),
                    "payment_currency:abc".to_string()
                )),
                "键 {key} 应被通配命中"
            );
        }
    }

    #[test]
    fn a_wildcard_still_fails_closed_on_two_params() {
        // 通配不放大歧义：两个联动参数时仍无法判定哪个携带父值
        let mapping = r#"{"*":{"parent_source_key":"c","parent_field":"p"}}"#;
        let error = linkage_filter_target(
            &request_with(Some(&[("a", "x"), ("b", "y")])),
            Some(mapping),
        )
        .err()
        .unwrap_or_else(|| panic!("两个参数必须 fail-closed"));
        assert_eq!(error.code, codes::LINKAGE_AMBIGUOUS);
    }

    #[test]
    fn an_exact_key_beats_the_wildcard() {
        let mapping = r#"{"*":{"parent_source_key":"wild","parent_field":"w"},
                          "widget1":{"parent_source_key":"exact","parent_field":"e"}}"#;
        let target = linkage_filter_target(
            &request_with(Some(&[("widget1", "exact:x")])),
            Some(mapping),
        )
        .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(target, Some(("exact".to_string(), "exact:x".to_string())));
    }

    #[test]
    fn one_matched_plus_one_unmatched_key_is_not_ambiguous() {
        // 多带一个无关键不该让请求失败：能确定唯一父级时应当照常工作
        let target = linkage_filter_target(
            &request_with(Some(&[("widget1", "payment_currency:x"), ("noise", "y")])),
            Some(MAPPING),
        )
        .unwrap_or_else(|_| panic!("不该产生业务失败"));
        assert_eq!(
            target,
            Some((
                "payment_currency".to_string(),
                "payment_currency:x".to_string()
            ))
        );
    }

    #[test]
    fn linkage_failure_codes_are_distinct_and_nonzero() {
        // 两个码必须可分辨：一个是「无法判定」，一个是「父值不存在」。
        // 混成一个会让控制台分不清「配置写错了」还是「飞书契约变了」。
        assert_ne!(codes::LINKAGE_AMBIGUOUS, codes::LINKAGE_NOT_RESOLVED);
        assert_ne!(codes::LINKAGE_AMBIGUOUS, 0);
        assert_ne!(codes::LINKAGE_NOT_RESOLVED, 0);
    }
}
