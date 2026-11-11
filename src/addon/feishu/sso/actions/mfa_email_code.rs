//! 已绑定用户的飞书登录：向 MFA 备用邮箱发送一次性验证码。
//!
//! 飞书身份已在 pending token 中验证，因此不需要像密码登录那样重新校验密码；
//! 只把验证码发到该用户**已验证**的邮箱。

use std::sync::Arc;

use yang_base::action::auth::{RegistrationEmailCodeAccepted, RegistrationEmailVerification};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::state;
use crate::addon::account::email_delivery::VerificationCodeSenderHandle;
use crate::addon::account::ExternalLoginPort;
use crate::config::MfaEmailVerificationConfig;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) MfaEmailCodeInput {
        token: Str::new().require(true).max_length(128),
    }
}

pub(super) fn register(
    module: ModuleSpec,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_mfa_email_code"),
            move |ctx, input| handle(ctx, input, Arc::clone(&external_login)),
        )
        .route(HttpMethod::Post, "/api/v1/auth/feishu/mfa/email-code")
        .display_name("飞书登录备用邮箱验证码")
        .description("向已绑定账号的已验证邮箱发送一次性第二因子验证码")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: MfaEmailCodeInput,
    external_login: Arc<dyn ExternalLoginPort>,
) -> Result<ApiResponse, BaseError> {
    let Some(nonce) = state::nonce_from_request(&ctx) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    let token = input.token.as_str();
    let now = crate::addon::feishu::domain::user_token::now_unix_secs();
    let Some(pending) = state::peek_pending_login(token, &nonce, now) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    let Some(email) = external_login.user_mfa_email(&ctx, pending.user_id).await? else {
        return Ok(ApiResponse::fail(
            40001,
            "该账号没有可用的备用邮箱，请改用认证器动态码或恢复码",
        ));
    };
    let mfa_config = ctx
        .tools()
        .config::<MfaEmailVerificationConfig>()
        .map_err(|_| {
            BaseError::ConfigError(
                "服务端未启用备用邮箱验证码（缺少 [email.mfa] 配置）".to_string(),
            )
        })?;
    let verification = RegistrationEmailVerification::from_config(mfa_config.engine_config())?;
    let sender = ctx.tools().extension::<VerificationCodeSenderHandle>()?;
    let accepted: RegistrationEmailCodeAccepted =
        verification.request_via(&ctx, &email, true, sender).await?;
    ApiResponse::success(accepted, "验证码已发送")
}
