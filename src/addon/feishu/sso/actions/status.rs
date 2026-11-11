//! 飞书 SSO 登录入口状态（登录页据此决定是否显示飞书按钮）。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct StatusInput {}

impl ParamInput for StatusInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_status"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/auth/feishu/status")
        .display_name("飞书登录状态")
        .description("查询飞书 SSO 登录入口是否可用")
        .public()
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: StatusInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let enabled = context
        .settings()
        .is_some_and(|settings| settings.login_configured());
    ApiResponse::success(serde_json::json!({ "enabled": enabled }), "ok")
}
