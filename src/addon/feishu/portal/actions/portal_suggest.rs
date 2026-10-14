//! 门户查询建议端点：按输入返回匹配的查询值候选列表，供前端自动补全。
//!
//! 与 `portal_query` 共享模糊匹配的粗筛 + 精筛逻辑（`fuzzy_like_key` +
//! `fuzzy_filter_candidates`），但只返回 label 列表，不做子列展开。
//!
//! # 为什么是 GET 而不是 POST
//!
//! 建议是幂等的只读查询，没有请求体（`query` 走 URL 参数），适合 GET + 浏览器缓存。
//! 与 `portal_metadata` 同一取舍：公开、匿名可访问、失败不消耗可用性预算。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

use super::portal_query::{
    extract_digits, find_portal, first_digit_run, fuzzy_filter_candidates, fuzzy_like_key,
};

/// 路由模板里的路径参数名（与 `portal_query` 同一约定）。
const SLUG_PARAM: &str = "slug";

/// 建议端点的路由。
const ROUTE: &str = "/api/v1/feishu/portal/{slug}/suggest";

/// 门户不存在 / 已停用（同码，不泄露存在性）。
const PORTAL_NOT_FOUND: i32 = 40401;

/// 建议结果最多返回的候选数。
const SUGGEST_LIMIT: usize = 20;

/// 建议端点的查询参数。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PortalSuggestInput {
    /// 查询关键词（URL 参数 `?query=xxx`）。
    #[serde(default)]
    pub(super) query: Option<String>,
}

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
    input: PortalSuggestInput,
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

    let Some(portal) = find_portal(&context, &slug).await? else {
        // 不泄露存在性：不存在的门户与没有建议的门户同回空列表。
        // 但停用的门户用 PORTAL_NOT_FOUND 码（与 portal_query 同一取舍）。
        return empty_response();
    };
    if !portal.optional::<bool>("enabled")?.unwrap_or(true) {
        return Ok(ApiResponse::fail(PORTAL_NOT_FOUND, "门户不存在"));
    }

    let fuzzy_enabled = portal.optional::<bool>("fuzzy_enabled")?.unwrap_or(true);
    let fuzzy_min_digits = portal.optional::<i64>("fuzzy_min_digits")?.unwrap_or(4) as usize;

    let query = input.query.unwrap_or_default();
    let query_stripped: String = query.chars().filter(|c| !c.is_whitespace()).collect();

    // 输入太短或模糊被禁用：不触发建议，返回空列表。
    let has_enough_digits = first_digit_run(&query_stripped, fuzzy_min_digits).is_some();
    if !fuzzy_enabled || !has_enough_digits || query_stripped.is_empty() {
        return empty_response();
    }

    let query_source_key: String = portal.require("query_field_source_key")?;
    let Some(like_key) = fuzzy_like_key(&query_stripped, fuzzy_min_digits) else {
        tracing::info!(query_stripped, fuzzy_min_digits, "suggest: fuzzy_like_key returned None");
        return empty_response();
    };
    let input_digits = extract_digits(&query_stripped);

    tracing::info!(
        query_source_key,
        like_key,
        input_digits,
        "suggest: querying options"
    );

    // 粗筛 + 精筛：与 portal_query 的 fuzzy_match 同一逻辑，但只取 label。
    let candidates = context
        .options()
        .query()
        .select_fields(&["option_id", "source_key", "label"])?
        .where_eq("source_key", serde_json::json!(&query_source_key))?
        .where_contains("label", &like_key)?
        .where_eq("enabled", serde_json::json!(true))?
        .page(1, SUGGEST_LIMIT)?
        .all()
        .await?;

    tracing::info!(
        candidate_count = candidates.len(),
        "suggest: DB query returned candidates"
    );

    let matched = fuzzy_filter_candidates(&candidates, &input_digits);

    tracing::info!(
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
    fn empty_input_parses_with_default_none() {
        let input = serde_json::from_value::<PortalSuggestInput>(serde_json::json!({}))
            .unwrap_or_else(|error| panic!("空对象应可解析: {error}"));
        assert!(input.query.is_none());
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
}
