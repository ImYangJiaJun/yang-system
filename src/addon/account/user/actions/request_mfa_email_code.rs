//! 请求登录 MFA 备用邮箱验证码（公开端点，等时密码校验防枚举）。
//!
//! 场景：账号已激活 TOTP 但认证器不可用（且未持有恢复码）时，改用注册邮箱
//! 接收一次性验证码完成登录第二因子。两段式登录的密码阶段是无状态的
//! （`SecondFactorRequired` 不签发任何凭据），因此本端点必须重新完成第一因子
//! 校验：密码错误返回与登录完全相同的 `InvalidPassword`，失败计数走
//! [`AuthOperation::Login`] 同一限流预算；密码正确但账号未激活 TOTP 或邮箱
//! 缺失时只消耗发送限额、不真实投递，对外仍返回统一 accepted 响应。

use super::login::record_login_failure;
use crate::addon::account::domain::policy::normalize_username;
use crate::addon::account::email_delivery::VerificationCodeSenderHandle;
use crate::addon::account::Account;
use crate::config::MfaEmailVerificationConfig;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, AuthOperation, BrowserSession, RegistrationEmailCodeAccepted,
    RegistrationEmailVerification,
};
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::transport::client_ip::client_ip_identity;
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestMfaEmailCodeInput {
    /// 用户名、邮箱或其他登录标识。
    username: String,
    /// 登录凭据。
    password: String,
}

impl ParamInput for RequestMfaEmailCodeInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RequestMfaEmailCodeInput,
    account: Arc<Account>,
) -> Result<RegistrationEmailCodeAccepted, BaseError> {
    BrowserSession::validate_same_origin(&ctx.request)?;
    // 与登录同一套标识分派与限流键（见 login.rs 的说明）：邮箱走唯一约束查询，
    // 其余按用户名；限流沿用归一化标识，防止跨维度绕过。
    let identifier = input.username.trim().to_ascii_lowercase();
    let limit_key = if identifier.contains('@') {
        normalize_email(&identifier)?
    } else {
        normalize_username(&identifier)?
    };
    account
        .rate_limiter()
        .check(&ctx, AuthOperation::Login, &limit_key)
        .await?;
    let user = if identifier.contains('@') {
        account
            .users()
            .find_credentials_by_email(&ctx, &limit_key)
            .await?
    } else {
        account
            .users()
            .find_credentials_by_username(&ctx, &limit_key)
            .await?
    };
    // 等时校验：用户不存在也执行一次完整 Argon2 校验，对外统一 InvalidPassword。
    let password_matches = account
        .passwords()
        .verify_or_dummy(
            &input.password,
            user.as_ref().map(|user| user.password_hash.as_str()),
        )
        .await?;
    if !password_matches {
        account
            .rate_limiter()
            .record_failure(&ctx, AuthOperation::Login, &limit_key)
            .await?;
        // 与登录失败同一事件面（粗粒度原因，best-effort 不阻塞响应）。
        let ip = client_ip_identity(&ctx).into_owned();
        let user_agent = ctx
            .request
            .get_header("user-agent")
            .unwrap_or_default()
            .to_string();
        if let Err(error) = record_login_failure(
            &ctx,
            &account,
            &input.username,
            &ip,
            &user_agent,
            "invalid_password",
        )
        .await
        {
            tracing::warn!(error = %error, "MFA 邮箱验证码请求的失败事件记录失败");
        }
        return Err(BaseError::InvalidPassword);
    }
    // verify_or_dummy 对 None 恒返回 false，能走到这里说明用户一定存在。
    let user = user.ok_or(BaseError::InvalidPassword)?;
    Account::ensure_active(user.status)?;

    let mfa_config = ctx
        .tools()
        .config::<MfaEmailVerificationConfig>()
        .map_err(|_| {
            BaseError::ConfigError(
                "服务端未启用邮箱验证码登录（缺少 [email.mfa] 配置）".to_string(),
            )
        })?;
    let verification = RegistrationEmailVerification::from_config(mfa_config.engine_config())?;
    let sender = ctx.tools().extension::<VerificationCodeSenderHandle>()?;

    // 只有 TOTP 已激活且持有已验证邮箱的账号才真实投递；其余只消耗限额、
    // 返回统一响应，不暴露账号的 MFA 状态。
    let state = account.users().find_totp_state_by_id(&ctx, user.id).await?;
    let deliver = state
        .as_ref()
        .is_some_and(|state| state.totp_activated_at.is_some() && state.email.is_some());
    let email = state.and_then(|state| state.email).unwrap_or_default();
    verification
        .request_via(&ctx, &email, deliver, sender)
        .await
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("request_mfa_email_code"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/mfa/email-code")
        .display_name("请求登录 MFA 邮箱验证码")
        .description("认证器不可用时向注册邮箱发送一次性登录验证码（等时密码校验防枚举）")
        .success_status(202)
        .public()
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_contract_requires_username_and_password() {
        assert!(serde_json::from_str::<RequestMfaEmailCodeInput>(
            r#"{"username":"alice","password":"secret"}"#
        )
        .is_ok());
        assert!(
            serde_json::from_str::<RequestMfaEmailCodeInput>(r#"{"username":"alice"}"#).is_err()
        );
        assert!(serde_json::from_str::<RequestMfaEmailCodeInput>(
            r#"{"username":"alice","password":"secret","extra":1}"#
        )
        .is_err());
    }
}
