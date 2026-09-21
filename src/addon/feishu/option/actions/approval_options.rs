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
use crate::addon::feishu::domain::i18n::{build_result_body, OptionRow, DEFAULT_LOCALE};
use crate::addon::feishu::domain::pagination::{decode_cursor, encode_cursor};
use crate::addon::feishu::domain::protocol::{FeishuEnvelope, FeishuOptionsRequest};
use crate::addon::feishu::domain::token::verify_token;

/// 单次请求返回的最大选项数。
///
/// 框架 `TableQuery` 的硬上限是 100（`MAX_QUERY_PAGE_SIZE`），飞书要求 page size
/// 不小于 10——100 同时满足两者。
const PAGE_SIZE: usize = 100;

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
}

/// 校验数据源与 Token；失败时给出信封。
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

    // 多取一行用于判定 hasMore
    let page = query.page(1, PAGE_SIZE + 1)?.paginate_records().await?;

    let mut rows = Vec::with_capacity(page.data.len());
    for record in page.data {
        rows.push(read_row(&record)?);
    }
    let has_more = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);

    // 游标必须用该行真实的 sort_order，否则下一页的 keyset 条件会错位
    let next_page_token = if has_more {
        rows.last()
            .map(|last| encode_cursor(last.sort_order, &last.option_id))
    } else {
        None
    };

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
}
