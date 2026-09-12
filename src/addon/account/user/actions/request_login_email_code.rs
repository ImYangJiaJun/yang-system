//! 请求邮箱验证码免密登录的一次性验证码（公开端点，统一 accepted 防枚举）。
//!
//! 与注册/换绑/MFA 验证码完全隔离的第四套 key 域（`[email.login]` 配置段）。
//! 邮箱命中已验证账号且账号启用时才真实投递；其余情况只消耗发送限额、
//! 不真实投递，对外仍返回统一 accepted 响应，不暴露邮箱是否注册或账号是否停用。

use crate::addon::account::email_delivery::LoginEmailCodeSenderHandle;
use crate::addon::account::Account;
use crate::config::LoginEmailVerificationConfig;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, AuthOperation, BrowserSession, RegistrationEmailCodeAccepted,
    RegistrationEmailVerification,
};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RequestLoginEmailCodeInput {
        /// 登录邮箱。
        email: Str::new()
            .title("登录邮箱")
            .require(true)
            .max_length(254)
            .email(),
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RequestLoginEmailCodeInput,
    account: Arc<Account>,
) -> Result<RegistrationEmailCodeAccepted, BaseError> {
    BrowserSession::validate_same_origin(&ctx.request)?;
    let email = normalize_email(&input.email)?;
    // 与密码登录共用 AuthOperation::Login 限流预算与归一化邮箱限流键。
    account
        .rate_limiter()
        .check(&ctx, AuthOperation::Login, &email)
        .await?;
    let login_config = ctx
        .tools()
        .config::<LoginEmailVerificationConfig>()
        .map_err(|_| {
            BaseError::ConfigError(
                "服务端未启用邮箱验证码登录（缺少 [email.login] 配置）".to_string(),
            )
        })?;
    let verification = RegistrationEmailVerification::from_config(login_config.engine_config())?;
    let sender = ctx.tools().extension::<LoginEmailCodeSenderHandle>()?;

    // 只有邮箱命中且账号启用时才真实投递；其余只消耗限额、返回统一响应，
    // 不暴露邮箱是否注册或账号是否停用。
    let user = account
        .users()
        .find_credentials_by_email(&ctx, &email)
        .await?;
    let deliver = user.as_ref().is_some_and(|user| user.status.is_active());
    verification
        .request_via(&ctx, &email, deliver, sender.engine())
        .await
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("request_login_email_code"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/login-email-code")
        .display_name("请求登录邮箱验证码")
        .description("向登录邮箱发送一次性免密登录验证码（统一 accepted 响应防枚举）")
        .success_status(202)
        .public()
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_email() {
        let params = <RequestLoginEmailCodeInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["email"]);
        assert!(params.as_slice().iter().all(|param| param.required));
        assert!(
            serde_json::from_value::<RequestLoginEmailCodeInput>(serde_json::json!({
                "email": "alice@example.com"
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<RequestLoginEmailCodeInput>(serde_json::json!({})).is_err()
        );
        assert!(
            serde_json::from_value::<RequestLoginEmailCodeInput>(serde_json::json!({
                "email": "alice@example.com",
                "extra": 1
            }))
            .is_err()
        );
    }
}
