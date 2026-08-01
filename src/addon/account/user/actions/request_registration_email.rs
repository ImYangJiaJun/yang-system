use super::super::email_verification::RegistrationEmailCodeAccepted;
use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RequestRegistrationEmailInput {
        email: Str::new()
            .title("注册邮箱")
            .require(true)
            .max_length(254)
            .email(),
    }
}

#[derive(Action)]
#[action(
    name = "request_registration_email",
    display_name = "发送注册邮箱验证码",
    description = "按邮箱、来源地址和全局容量限制投递一次性注册验证码",
    method = "POST",
    path = "/api/v1/users/registration-email-verifications",
    success_status = 202,
    public
)]
struct RequestRegistrationEmailAction {
    service: Arc<UserService>,
}

#[async_trait]
impl ActionHandler for RequestRegistrationEmailAction {
    type Input = RequestRegistrationEmailInput;
    type Output = RegistrationEmailCodeAccepted;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.service
            .request_registration_email(&ctx, &input.email)
            .await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    Ok(module.native_action(RequestRegistrationEmailAction { service }))
}
