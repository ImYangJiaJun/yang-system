//! 轮换 Refresh Token 并签发新 Token 对。

use super::super::domain::auth_adapters::UserClaimsResolver;
use super::super::domain::service::UserService;
use std::sync::Arc;
use yang_base::action::auth::{BrowserSession, RefreshAction, RefreshInput};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserRefreshInput {}

impl ParamInput for BrowserRefreshInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: BrowserRefreshInput,
    service: Arc<UserService>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let refresh_token = super::super::browser_session().refresh_token(&ctx.request)?;
    let tokens = RefreshAction::new(UserClaimsResolver::new(service))
        .handle(ctx, RefreshInput { refresh_token })
        .await?;
    super::super::browser_session().token_response(
        tokens.access_token,
        tokens.refresh_token,
        secure,
    )
}
