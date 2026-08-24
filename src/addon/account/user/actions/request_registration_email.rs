//! 按邮箱、来源地址和全局容量限制投递一次性注册验证码。

use crate::addon::account::Account;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, RegistrationEmailCodeAccepted, RegistrationEmailVerification,
};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
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
    account: Arc<Account>,
) -> Result<RegistrationEmailCodeAccepted, BaseError> {
    let email = normalize_email(&input.email)?;
    // 已注册邮箱不投递验证码，但对调用方返回一致响应，避免枚举已注册邮箱。
    let deliver = !account.users().email_exists(&ctx, &email).await?;
    RegistrationEmailVerification::from_context(&ctx)?
        .request(&ctx, &email, deliver)
        .await
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("request_registration_email"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/users/registration-email-verifications",
        )
        .display_name("发送注册邮箱验证码")
        .description("按邮箱、来源地址和全局容量限制投递一次性注册验证码")
        .success_status(202)
        .public()
        .register()
}
