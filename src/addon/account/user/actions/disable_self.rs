//! 停用当前账号及全部平台/企业关系，并撤销此前签发的全部会话。

use super::super::domain::service::UserService;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::auth::BrowserSession;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DisableSelfInput {}

impl ParamInput for DisableSelfInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DisableSelfResult {
    account_disabled: bool,
    immediate_convergence: bool,
    relogin_required: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: DisableSelfInput,
    service: Arc<UserService>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let user_id = ctx.actor()?.user_id();
    let immediate_convergence = service.disable_self(&ctx, user_id).await?;
    super::super::browser_session().clear_response(
        ApiResponse::success(
            DisableSelfResult {
                account_disabled: true,
                immediate_convergence,
                relogin_required: true,
            },
            if immediate_convergence {
                "账号已停用，全部会话已撤销"
            } else {
                "账号已停用，Redis 即时收敛待后台重试"
            },
        )?,
        secure,
    )
}
