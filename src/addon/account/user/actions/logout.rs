//! 递增持久会话版本并撤销当前账号此前签发的全部 Access 与 Refresh Token。

use super::super::domain::service::UserService;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::auth::BrowserSession;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserLogoutInput {}

impl ParamInput for BrowserLogoutInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct BrowserLogoutResult {
    revoked_all_sessions: bool,
    immediate_convergence: bool,
    relogin_required: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: BrowserLogoutInput,
    service: Arc<UserService>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx.actor()?.user_id();
    let immediate_convergence = service.revoke_all_sessions(&ctx, user_id).await?;
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
