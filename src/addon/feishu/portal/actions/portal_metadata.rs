//! 门户公开元信息端点：公开查询页渲染标题与查询键提示所需的只读数据。
//!
//! # 为什么单独一个端点
//!
//! `portal_query` 只在**查询之后**才回数据，而页面在第一次查询之前就要显示门户标题
//! 与「该填什么」的提示。这些是配置而不是查询结果，所以单独取。
//!
//! # 为什么是 `public`
//!
//! 与 `portal_query` 同一取舍：三种访问模式里只有 `login` 依赖框架身份，而元信息
//! 本身不含任何数据行，公开取没有泄露面——口令 / 登录判权仍然只发生在 `portal_query`
//! 上。停用与不存在的门户一律回同一码，不泄露「这个 slug 存在但被停用」。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

use super::portal_query::find_portal;

/// 路由模板里的路径参数名（与 `portal_query` 同一约定）。
const SLUG_PARAM: &str = "slug";

/// 公开元信息端点的路由。
const ROUTE: &str = "/api/v1/feishu/portal/{slug}";

/// 门户不存在 / 已停用（同码，不泄露存在性）。
const PORTAL_NOT_FOUND: i32 = 40401;

/// 元信息端点无请求体（`slug` 走路径参数）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PortalMetadataInput {}

impl ParamInput for PortalMetadataInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 公开页渲染所需的门户元信息。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PortalMetadataOutput {
    /// 门户标题。
    title: String,
    /// 查询键的展示名（`feishu_datasource_field.field_name`）。
    query_field_label: String,
    /// 访问模式（public / passphrase / login）。
    access_mode: String,
    /// 查无结果时的提示文案；未配置时为空串。
    empty_message: String,
}

/// 注册公开元信息端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("portal_metadata"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, ROUTE)
        .display_name("门户元信息")
        .description("按 slug 取门户的公开元信息：标题、查询键展示名、访问模式、空结果提示")
        // public：元信息不含数据行，判权仍只发生在 portal_query 上。
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: PortalMetadataInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let slug = ctx
        .request
        .get_path_param(SLUG_PARAM)
        .unwrap_or_default()
        .trim()
        .to_string();
    if slug.is_empty() {
        return Ok(ApiResponse::fail(PORTAL_NOT_FOUND, "门户不存在"));
    }

    let Some(portal) = find_portal(&context, &slug).await? else {
        return Ok(ApiResponse::fail(PORTAL_NOT_FOUND, "门户不存在"));
    };
    if !portal.optional::<bool>("enabled")?.unwrap_or(true) {
        return Ok(ApiResponse::fail(PORTAL_NOT_FOUND, "门户不存在"));
    }

    let query_field_id: String = portal.require("query_field_id")?;
    let query_field_label = load_field_label(
        &context,
        portal.require("datasource_id")?,
        &query_field_id,
    )
    .await?;

    ApiResponse::success(
        PortalMetadataOutput {
            title: portal.optional::<String>("title")?.unwrap_or_default(),
            query_field_label,
            access_mode: portal
                .optional::<String>("access_mode")?
                .unwrap_or_else(|| "public".to_string()),
            empty_message: portal.optional::<String>("empty_message")?.unwrap_or_default(),
        },
        "门户元信息",
    )
}

/// 取查询键的展示名。绑定缺失或名字为空（配置残缺）时退回 `field_id`，页面至少不空。
async fn load_field_label(
    context: &FeishuContext,
    datasource_id: i64,
    field_id: &str,
) -> Result<String, BaseError> {
    let row = context
        .datasource_fields()
        .query()
        .select_fields(&["field_name"])?
        .where_eq("datasource_id", serde_json::json!(datasource_id))?
        .where_eq("field_id", serde_json::json!(field_id))?
        .optional()
        .await?;
    let Some(row) = row else {
        return Ok(field_id.to_string());
    };
    let name = row.optional::<String>("field_name")?.unwrap_or_default();
    Ok(if name.trim().is_empty() {
        field_id.to_string()
    } else {
        name
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_route_carries_the_path_parameter_the_handler_reads() {
        // 与 `portal_query` 同一道防线：路由模板里的参数名与 `get_path_param` 的实参
        // 必须同一个来源，写错一处端点对任何请求都回 404，且不在启动期暴露。
        assert_eq!(SLUG_PARAM, "slug");
        assert!(
            ROUTE.contains(&format!("{{{SLUG_PARAM}}}")),
            "路由必须带 {{{SLUG_PARAM}}} 路径参数，实际是 {ROUTE}"
        );
    }

    #[test]
    fn the_metadata_route_does_not_collide_with_the_query_route() {
        // 同一条公开路径前缀上挂两个端点：方法不同（GET 取元信息 / POST 查询），
        // 路径也不同（后者多一段 `/query`），分派不歧义。
        assert_eq!(ROUTE, "/api/v1/feishu/portal/{slug}");
    }

    #[test]
    fn the_output_serializes_the_contract_the_page_reads() {
        let output = PortalMetadataOutput {
            title: "卡号查询".to_string(),
            query_field_label: "卡号".to_string(),
            access_mode: "passphrase".to_string(),
            empty_message: "未找到匹配记录".to_string(),
        };
        let value = serde_json::to_value(output).unwrap_or_else(|e| panic!("应可序列化: {e}"));
        assert_eq!(value["title"], serde_json::json!("卡号查询"));
        assert_eq!(value["query_field_label"], serde_json::json!("卡号"));
        assert_eq!(value["access_mode"], serde_json::json!("passphrase"));
        assert_eq!(value["empty_message"], serde_json::json!("未找到匹配记录"));
    }
}
