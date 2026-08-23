//! 使用短期单次凭证重置密码；请求不依赖现有登录会话。

use super::super::domain::policy::{PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH};
use super::super::domain::service::UserService;
use std::sync::Arc;
use yang_base::action::auth::BrowserSession;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{Password, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ResetPasswordInput {
        reset_token: Str::new()
            .title("密码重置凭证")
            .require(true)
            .min_length(1)
            .max_length(256),
        new_password: Password::new()
            .title("新密码")
            .require(true)
            .min_length(PASSWORD_MIN_LENGTH)
            .max_length(PASSWORD_MAX_LENGTH),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ResetPasswordInput,
    service: Arc<UserService>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    service
        .reset_password(&ctx, &input.reset_token, &input.new_password)
        .await?;
    super::super::browser_session().relogin_response("密码已重置，请使用新密码登录", secure)
}
