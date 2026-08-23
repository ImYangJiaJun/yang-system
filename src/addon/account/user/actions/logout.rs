use super::super::service::UserService;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::auth::BrowserSession;
use yang_base::action::{Action as BusinessAction, ActionContext, ApiResponse};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BrowserLogoutInput {}

impl ParamInput for BrowserLogoutInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(yang_base::Action)]
#[action(
    name = "logout",
    display_name = "退出全部会话",
    description = "递增持久会话版本并撤销当前账号此前签发的全部 Access 与 Refresh Token",
    method = "POST",
    path = "/api/v1/users/logout"
)]
struct BrowserLogoutAction {
    service: Arc<UserService>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct BrowserLogoutResult {
    revoked_all_sessions: bool,
    immediate_convergence: bool,
    relogin_required: bool,
}

#[async_trait]
impl BusinessAction for BrowserLogoutAction {
    type Input = BrowserLogoutInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = BrowserSession::validate_same_origin(&ctx.request)?;
        let user_id = ctx.actor()?.user_id();
        let immediate_convergence = self.service.revoke_all_sessions(&ctx, user_id).await?;
        super::super::browser_session().clear_response(
            ApiResponse::success(
                BrowserLogoutResult {
                    revoked_all_sessions: true,
                    immediate_convergence,
                    relogin_required: true,
                },
                if immediate_convergence {
                    "已撤销全部会话"
                } else {
                    "持久会话已撤销，Redis 即时收敛待后台重试"
                },
            )?,
            secure,
        )
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(BrowserLogoutAction { service }))
}
