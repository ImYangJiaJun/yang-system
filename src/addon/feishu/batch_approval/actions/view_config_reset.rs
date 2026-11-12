//! 重置当前用户在指定审批定义下的个人视图。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::domain::view;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ResetViewInput {
    approval_code: String,
}

impl ParamInput for ResetViewInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只重置自己的批量审批视图
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_view_config_reset"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/batch-approval/view-config/reset",
        )
        .display_name("重置批量审批视图配置")
        .description("删除当前用户在指定审批定义下的个人视图")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ResetViewInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let code = view::normalize_code(&input.approval_code)?;
    view::reset(&context, user_id, &code).await?;
    let event = audit::succeeded_event(
        &ctx,
        None,
        Some(audit::entity("user", user_id)?),
        audit::entity("feishu_batch_user_view", format!("{user_id}:{code}"))?,
        None,
        Some(audit::summary([
            ("approval_code", json!(code)),
            ("reset", json!(true)),
        ])?),
    )?;
    audit::append_independent(ctx.tools().mysql()?.pool(), &event).await?;
    ApiResponse::success(json!({ "reset": true }), "视图配置已重置")
}
