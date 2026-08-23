//! 按邮箱、来源地址和全局容量限制投递一次性注册验证码。

use super::super::domain::service::UserService;
use std::sync::Arc;
use yang_base::action::auth::RegistrationEmailCodeAccepted;
use yang_base::action::ActionContext;
use yang_base::definition::Str;
use yang_base::BaseError;

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

pub(super) async fn handle(
    ctx: ActionContext,
    input: RequestRegistrationEmailInput,
    service: Arc<UserService>,
) -> Result<RegistrationEmailCodeAccepted, BaseError> {
    service.request_registration_email(&ctx, &input.email).await
}
