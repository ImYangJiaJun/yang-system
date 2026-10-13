//! 门户查询结果导出（Phase 3 占位）。
//!
//! 设计文档把导出划到 Phase 3（开放决策点 D-3），本批只落地路由与输入契约，
//! 让前端可以先把入口挂上、后端给出可归因的「尚未实现」而不是 404。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::ApiResponse;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

/// 导出请求的输入契约。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ExportPortalResultsInput {
    /// 目标门户 `id`。
    pub(super) portal_id: i64,
    /// 查询值（与 `portal_query` 同一口径，精确匹配）。
    pub(super) value: String,
}

impl ParamInput for ExportPortalResultsInput {
    fn params() -> Params {
        Params::new()
    }
}

impl ExportPortalResultsInput {
    fn validate(&self) -> Result<(), BaseError> {
        if self.portal_id <= 0 {
            return Err(BaseError::ParamInvalid(
                "portal_id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if self.value.trim().is_empty() {
            return Err(BaseError::ParamInvalid(
                "value".to_string(),
                "查询值不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 注册导出端点。
pub(super) fn register(module: ModuleSpec, _context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("export_portal_results"),
            move |_ctx, input| handle(input),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/portals/export")
        .display_name("导出门户查询结果")
        .description("导出门户查询结果（尚未实现）")
        .permissions(["feishu.portal.read"])
        .register()
}

pub(super) async fn handle(input: ExportPortalResultsInput) -> Result<ApiResponse, BaseError> {
    input.validate()?;
    Ok(ApiResponse::fail(50101, "尚未实现"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_non_positive_portal_id() {
        let input = ExportPortalResultsInput {
            portal_id: 0,
            value: "6222".to_string(),
        };
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_a_blank_value() {
        let input = ExportPortalResultsInput {
            portal_id: 1,
            value: "  ".to_string(),
        };
        assert!(input.validate().is_err());
    }
}
