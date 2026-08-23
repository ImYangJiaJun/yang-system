//! 校验账号密码并签发 Token。

use super::super::domain::auth_adapters::UserCredentialVerifier;
use super::super::domain::service::UserService;
use std::sync::Arc;
use yang_base::action::auth::{BrowserSession, LoginAction, LoginInput};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct BrowserLoginInput {
    /// 用户名、邮箱或其他登录标识。
    username: String,
    /// 登录凭据。
    password: String,
    /// 可选业务扩展字段。
    #[serde(default)]
    extra: serde_json::Value,
}

impl ParamInput for BrowserLoginInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: BrowserLoginInput,
    service: Arc<UserService>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    let tokens = LoginAction::new(UserCredentialVerifier::new(service))
        .handle(
            ctx,
            LoginInput {
                username: input.username,
                password: input.password,
                extra: input.extra,
            },
        )
        .await?;
    super::super::browser_session().token_response(
        tokens.access_token,
        tokens.refresh_token,
        secure,
    )
}
