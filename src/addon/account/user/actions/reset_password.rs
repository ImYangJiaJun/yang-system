//! 使用短期单次凭证重置密码；请求不依赖现有登录会话。

use super::super::policy::{PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH};
use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext, ApiResponse};
use yang_base::definition::{ModuleSpec, Password, Str};
use yang_base::{Action, BaseError};

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

#[derive(Action)]
#[action(
    name = "reset_password",
    display_name = "重置密码",
    description = "消费短期单次凭证并使已有会话失效",
    method = "POST",
    path = "/api/v1/users/reset-password",
    public
)]
struct ResetPasswordAction {
    service: Arc<UserService>,
}

#[async_trait]
impl ActionHandler for ResetPasswordAction {
    type Input = ResetPasswordInput;
    type Output = ApiResponse;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let secure = super::super::browser_session::validate_same_origin(&ctx.request)?;
        self.service
            .reset_password(&ctx, &input.reset_token, &input.new_password)
            .await?;
        super::super::browser_session::relogin_response("密码已重置，请使用新密码登录", secure)
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(ResetPasswordAction { service }))
}
