//! 门户查询建议端点：按输入返回匹配的查询值候选列表，供前端自动补全。
//!
//! 与 `portal_query` 共享模糊匹配的粗筛 + 精筛逻辑（`fuzzy_like_key` +
//! `fuzzy_filter_candidates`），但只返回 label 列表，不做子列展开。
//!
//! # 为什么是 GET 而不是 POST
//!
//! 建议是幂等的只读查询，没有请求体（`query` 走 URL 参数），适合 GET + 浏览器缓存。
//! 与 `portal_metadata` 同一取舍：端点仍注册为 public，但 handler 复用
//! `portal_query` 的 `access_mode` 判权与按门户主键的限速——public 门户匿名可用，
//! login 门户要求已登录，passphrase 门户直接 403（建议值走 GET，不应把口令放进 URL）。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::rate_limit;
use yang_base::transport::client_ip::client_ip_identity;

use super::portal_query::{
    authorize, coarse_pattern, codes, extract_digits, find_portal, first_digit_run,
    fuzzy_filter_candidates,
};

/// 路由模板里的路径参数名（与 `portal_query` 同一约定）。
const SLUG_PARAM: &str = "slug";

/// 建议端点的路由。
const ROUTE: &str = "/api/v1/feishu/portal/{slug}/suggest";

/// 门户不存在 / 已停用（同码，不泄露存在性）。
const PORTAL_NOT_FOUND: i32 = 40401;

/// 建议结果最多返回的候选数。
const SUGGEST_LIMIT: usize = 20;

/// 建议端点的输入。
///
/// # 为什么是空结构体（这里踩过坑）
///
/// 框架的 Action 输入**只从请求体反序列化**（`ActionContext::extract_input_owned`），
/// URL query 参数被存在 `ctx.request` 的 query map 里、从不进 input 结构体。本端点
/// 上线首日声明了 `query: Option<String>` 字段，于是 `?query=4514` 恒反序列化成
/// `None`，对任何输入都回空建议——端点「注册成功、响应 200、永远空」。`query` 值
/// 只能经 `ctx.request.get_query("query")` 读取（与 `portal_metadata` 读 slug 同一模式）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PortalSuggestInput {}

impl ParamInput for PortalSuggestInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 注册建议端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("portal_suggest"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, ROUTE)
        .display_name("门户查询建议")
        .description("按输入返回匹配的查询值候选列表，供前端自动补全")
        // public：与 portal_query / portal_metadata 同一取舍。
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: PortalSuggestInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let slug = ctx
        .request
        .get_path_param(SLUG_PARAM)
        .unwrap_or_default()
        .trim()
        .to_string();
    if slug.is_empty() {
        return empty_response();
    }
    // URL query 参数不走 Action 输入（见 PortalSuggestInput 的说明）。
    let query = ctx
        .request
        .get_query("query")
        .unwrap_or_default()
        .to_string();

    let Some(portal) = find_portal(&context, &slug).await? else {
        // 不泄露存在性：不存在的门户与没有建议的门户同回空列表。
        // 但停用的门户用 PORTAL_NOT_FOUND 码（与 portal_query 同一取舍）。
        return empty_response();
    };
    if !portal.optional::<bool>("enabled")?.unwrap_or(true) {
        return Ok(ApiResponse::fail(PORTAL_NOT_FOUND, "门户不存在"));
    }

    // 与 portal_query 同一套判权与限速：suggest 返回的是查询键的真实取值，
    // 不能成为绕过 access_mode 的匿名枚举入口。passphrase 门户在这里直接 403
    // （建议值走 GET，不应把口令放进 URL）；login 门户要求已登录。
    let portal_id: i64 = portal.require("id")?;
    let access_mode: String = portal
        .optional("access_mode")?
        .unwrap_or_else(|| "public".to_string());
    let client_ip: String = client_ip_identity(&ctx).chars().take(45).collect();
    let limit = portal
        .optional::<i64>("rate_limit_per_min")?
        .unwrap_or(30)
        .clamp(1, i64::from(u32::MAX)) as u32;
    if let Err(error) = rate_limit::check_query(&ctx, portal_id, &client_ip, limit).await {
        return Err(error);
    }
    if let Err(reason) = authorize(
        &ctx,
        &context,
        &access_mode,
        None,
        portal.optional::<String>("passphrase_hash")?.as_deref(),
        portal.optional::<i64>("passphrase_expires_at")?,
    )
    .await?
    {
        return Ok(ApiResponse::fail(codes::FORBIDDEN, reason));
    }

    let fuzzy_enabled = portal.optional::<bool>("fuzzy_enabled")?.unwrap_or(true);
    let fuzzy_min_digits = portal.optional::<i64>("fuzzy_min_digits")?.unwrap_or(4) as usize;

    let query_stripped: String = query.chars().filter(|c| !c.is_whitespace()).collect();

    // 输入太短或模糊被禁用：不触发建议，返回空列表。
    let has_enough_digits = first_digit_run(&query_stripped, fuzzy_min_digits).is_some();
    if !fuzzy_enabled || !has_enough_digits || query_stripped.is_empty() {
        return empty_response();
    }

    let query_source_key: String = portal.require("query_field_source_key")?;
    let input_digits = extract_digits(&query_stripped);
    let Some(pattern) = coarse_pattern(&input_digits) else {
        return empty_response();
    };

    tracing::debug!(
        query_source_key,
        pattern,
        input_digits,
        "suggest: querying options"
    );

    // 粗筛 + 精筛：粗筛的分块 LIKE 模式随输入位数收窄（见 `coarse_pattern`），
    // 精筛在 Rust 里按数字子串核对（与旧 fuzzy_match 同一判据）。
    let candidates = context
        .options()
        .query()
        .select_fields(&["option_id", "source_key", "label"])?
        .where_eq("source_key", serde_json::json!(&query_source_key))?
        .where_like("label", pattern)?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, SUGGEST_LIMIT)?
        .all()
        .await?;

    tracing::debug!(
        candidate_count = candidates.len(),
        "suggest: DB query returned candidates"
    );

    let matched = fuzzy_filter_candidates(&candidates, &input_digits);

    tracing::debug!(
        matched_count = matched.len(),
        "suggest: after Rust-side filter"
    );
    let suggestions: Vec<String> = matched
        .iter()
        .filter_map(|record| record.optional::<String>("label").ok().flatten())
        .take(SUGGEST_LIMIT)
        .collect();

    Ok(ApiResponse::success_value(
        serde_json::json!({"suggestions": suggestions}),
        "建议列表",
    ))
}

/// 空建议列表响应。
fn empty_response() -> Result<ApiResponse, BaseError> {
    Ok(ApiResponse::success_value(
        serde_json::json!({"suggestions": []}),
        "建议列表",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_route_carries_the_path_parameter_the_handler_reads() {
        assert_eq!(SLUG_PARAM, "slug");
        assert!(
            ROUTE.contains(&format!("{{{SLUG_PARAM}}}")),
            "路由必须带 {{{SLUG_PARAM}}} 路径参数，实际是 {ROUTE}"
        );
    }

    #[test]
    fn the_suggest_route_is_under_the_portal_path_prefix() {
        // 与 portal_query / portal_metadata 同一公开路径前缀。
        assert!(ROUTE.starts_with("/api/v1/feishu/portal/"));
        assert!(ROUTE.ends_with("/suggest"));
    }

    #[test]
    fn empty_input_parses_the_empty_body_the_framework_sends_for_get() {
        // GET 请求无请求体时框架把 body 折成 `{}`；空结构体必须能解析它，
        // 否则端点会在反序列化阶段 400（而不是走到 handler）。
        serde_json::from_value::<PortalSuggestInput>(serde_json::json!({}))
            .unwrap_or_else(|error| panic!("空对象应可解析: {error}"));
    }

    #[test]
    fn deny_unknown_fields_catches_typos() {
        // 与 portal_query 同一防线：拼错的键必须响亮失败。
        assert!(
            serde_json::from_value::<PortalSuggestInput>(serde_json::json!({
                "querry": "1234"
            }))
            .is_err()
        );
    }

    #[test]
    fn the_query_value_is_read_from_the_url_query_string_not_the_body() {
        // 本端点上线首日返回恒空建议的根因回归测试：query 值必须能从 URL query
        // string 读到。框架的 Action 输入只从请求体反序列化，query string 参数
        // 只能经 `Request::get_query` 读取——handler 读的就是这个入口。
        let request =
            yang_base::action::Request::new(serde_json::json!({})).query("query", "4514 6176");
        assert_eq!(request.get_query("query"), Some("4514 6176"));
        // 空白键与缺失键都回 None（不 panic）。
        assert_eq!(request.get_query(""), None);
        assert_eq!(request.get_query("missing"), None);
    }
}
