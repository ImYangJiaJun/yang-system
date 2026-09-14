//! 邮箱验证码免密登录（公开端点；验证码 Redis 原子单次消费，防枚举统一错误）。
//!
//! 复用框架 `LoginAction` 的签发路径：`LoginInput.username` 承载邮箱、
//! `LoginInput.password` 承载验证码，由 [`EmailCodeCredentialVerifier`] 完成
//! 限流、验证码校验、账号存在性与状态校验；签发后会话落库、登录事件与
//! 新设备邮件提醒与密码登录完全同路径（复用 login.rs 的共享函数）。
//!
//! 两段式 MFA（多因子任选登录方案阶段 1，消灭 G-1 缺口）：账号已激活 TOTP 时，
//! 第一段提交邮箱+验证码只验不消费（`verify_only`），返回 `SecondFactorRequired`；
//! 第二段重发邮箱+同一验证码+`extra.mfa_code`，先原子消费验证码再验第二因子。
//! 第一因子已是邮箱持有，第二因子严格禁用 `[email.mfa]` 备用邮箱通道
//! （同类不构成双因子，方案 D-2），只接受 TOTP / 恢复码。

use super::login::{record_login_failure, record_login_session};
use crate::addon::account::Account;
use crate::config::LoginEmailVerificationConfig;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, AuthOperation, BrowserSession, CredentialVerifier, LoginAction, LoginInput,
    RegistrationEmailVerification, VerifiedSubject,
};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::transport::client_ip::client_ip_identity;
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) LoginByEmailCodeInput {
        /// 登录邮箱。
        email: Str::new()
            .title("登录邮箱")
            .require(true)
            .max_length(254)
            .email(),
        /// 邮箱一次性验证码（6 位数字）。
        email_code: Str::new()
            .title("邮箱验证码")
            .require(true)
            .min_length(6)
            .max_length(6)
            .pattern(r"^[0-9]{6}$"),
        /// 第二因子验证码（账号已激活 TOTP 时第二段提交：TOTP 动态码或一次性恢复码）。
        mfa_code: Str::new()
            .title("双重验证码")
            .require(false)
            .min_length(6)
            .max_length(64),
    }
}

/// 与框架验证码引擎 consume 失败完全一致的统一错误（防枚举）。
fn invalid_email_code() -> BaseError {
    BaseError::ParamInvalid(
        "email_code".to_string(),
        "邮箱验证码无效或已过期".to_string(),
    )
}

/// 把邮箱验证码校验接到框架内置 `LoginAction` 的端口。
#[derive(Clone)]
struct EmailCodeCredentialVerifier {
    account: Arc<Account>,
}

#[async_trait]
impl CredentialVerifier for EmailCodeCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let email = normalize_email(input.username.trim())?;
        self.account
            .rate_limiter()
            .check(ctx, AuthOperation::Login, &email)
            .await?;
        let login_config = ctx
            .tools()
            .config::<LoginEmailVerificationConfig>()
            .map_err(|_| {
                BaseError::ConfigError(
                    "服务端未启用邮箱验证码登录（缺少 [email.login] 配置）".to_string(),
                )
            })?;
        let verification =
            RegistrationEmailVerification::from_config(login_config.engine_config())?;
        let mfa_code = input
            .extra
            .get("mfa_code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        // 两段式与验证码单次消费的调和（方案 A）：第一段（无 mfa_code）只验
        // 不消费，验证码在 TTL 内留给第二段正式消费。peek 失败计一次限流失败，
        // 返回统一无效验证码错误（防枚举边界与密码登录一致）。
        if let Err(error) = verification.verify_only(ctx, &email, &input.password).await {
            self.account
                .rate_limiter()
                .record_failure(ctx, AuthOperation::Login, &email)
                .await?;
            return Err(error);
        }
        // 验证码有效；账号不存在或停用仍返回统一无效验证码错误，不泄露账号状态
        // （发码端点只对已注册且启用的账号真实投递，其余邮箱不存在有效验证码）。
        let user = self
            .account
            .users()
            .find_credentials_by_email(ctx, &email)
            .await?
            .ok_or_else(invalid_email_code)?;
        if !user.status.is_active() {
            return Err(invalid_email_code());
        }
        // 账号激活 TOTP 时登录为两段式（与密码登录同语义）：
        // 缺 mfa_code 的第一段是协议步骤而非攻击信号，不计失败、不消费验证码。
        let totp_state = self
            .account
            .users()
            .find_totp_state_by_id(ctx, user.id)
            .await?;
        let totp_activated = totp_state
            .as_ref()
            .is_some_and(|state| state.totp_activated_at.is_some());
        if totp_activated && mfa_code.is_none() {
            return Err(BaseError::SecondFactorRequired);
        }
        // 签发凭据前必须原子消费验证码（verify_only 的契约要求）；此后验证码
        // 失效，失败只可能来自第二因子。消费失败计一次限流失败。
        if let Err(error) = verification.consume(ctx, &email, &input.password).await {
            self.account
                .rate_limiter()
                .record_failure(ctx, AuthOperation::Login, &email)
                .await?;
            return Err(error);
        }
        if totp_activated {
            if let (Some(state), Some(code)) = (totp_state, mfa_code) {
                let secret = self
                    .account
                    .users()
                    .decrypt_totp_secret(ctx, &state)
                    .await?;
                // 第一因子已是邮箱持有：备用邮箱通道禁用，只接受 TOTP / 恢复码。
                let accepted = self
                    .account
                    .verify_second_factor(ctx, user.id, &state, &secret, &code, false)
                    .await
                    .is_ok();
                if !accepted {
                    self.account
                        .rate_limiter()
                        .record_failure(ctx, AuthOperation::Login, &email)
                        .await?;
                    return Err(BaseError::ParamInvalid(
                        "mfa_code".to_string(),
                        "双重验证码错误或已过期".to_string(),
                    ));
                }
            }
        }
        let claims = self.account.claims_for(ctx, user.id).await?;
        Ok(VerifiedSubject::new(user.id.to_string()).with_token_pair_claims(claims))
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: LoginByEmailCodeInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    // 会话持久化所需信息在 ctx 被 move 进 LoginAction 前提取（与 login.rs 同构）。
    let session_ip = client_ip_identity(&ctx).into_owned();
    let session_user_agent = ctx
        .request
        .get_header("user-agent")
        .unwrap_or_default()
        .to_string();
    let record_ctx = ctx.clone();
    let login_result = LoginAction::new(EmailCodeCredentialVerifier {
        account: Arc::clone(&account),
    })
    .handle(
        ctx,
        LoginInput {
            username: input.email.clone(),
            password: input.email_code,
            extra: input
                .mfa_code
                .as_ref()
                .map(|code| serde_json::json!({ "mfa_code": code }))
                .unwrap_or(serde_json::Value::Null),
        },
    )
    .await;
    // 失败路径 best-effort 记录登录事件（粗粒度原因，不记录明文）；
    // 成功路径由 record_login_session 在会话落库时一并记录。
    let tokens = match login_result {
        Ok(tokens) => tokens,
        Err(error) => {
            // 两段式登录的第一阶段通过（等待第二因子输入）不是失败事件，不记录。
            if !matches!(error, BaseError::SecondFactorRequired) {
                // 按错误类型映射粗粒度失败原因（用户不存在由 record_login_failure 二次判定）。
                let failure_reason = match &error {
                    BaseError::RateLimitExceeded { .. } => "rate_limited",
                    BaseError::Unauthorized(_) => "disabled",
                    _ => "invalid_password",
                };
                if let Err(record_error) = record_login_failure(
                    &record_ctx,
                    &account,
                    &input.email,
                    &session_ip,
                    &session_user_agent,
                    failure_reason,
                )
                .await
                {
                    tracing::warn!(error = %record_error, "邮箱验证码登录失败事件记录失败");
                }
            }
            return Err(error);
        }
    };
    if let Err(error) = record_login_session(
        &record_ctx,
        &account,
        &tokens.access_token,
        &tokens.refresh_token,
        &session_ip,
        &session_user_agent,
    )
    .await
    {
        tracing::warn!(error = %error, "邮箱验证码登录会话持久化失败");
    }
    Account::browser_session().token_response(tokens.access_token, tokens.refresh_token, secure)
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("login_by_email_code"),
            move |ctx, input| handle(ctx, input, Arc::clone(&account)),
        )
        .route(HttpMethod::Post, "/api/v1/users/login-by-email-code")
        .display_name("邮箱验证码登录")
        .description("校验邮箱一次性验证码并签发 Token（免密登录，两段式 MFA，验证码单次消费）")
        .public()
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_email_and_code() {
        let params = <LoginByEmailCodeInput as ParamInput>::params();
        let names = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["email", "email_code", "mfa_code"]);
        assert!(params.as_slice().iter().take(2).all(|param| param.required));
        assert!(!params.as_slice()[2].required);
    }

    #[test]
    fn input_rejects_unknown_fields_and_missing_code() {
        assert!(
            serde_json::from_value::<LoginByEmailCodeInput>(serde_json::json!({
                "email": "alice@example.com",
                "email_code": "123456"
            }))
            .is_ok()
        );
        // 两段式第二段：可选 mfa_code 入参。
        let second_segment = serde_json::from_value::<LoginByEmailCodeInput>(serde_json::json!({
            "email": "alice@example.com",
            "email_code": "123456",
            "mfa_code": "654321"
        }))
        .unwrap_or_else(|error| panic!("带 mfa_code 的第二段输入应合法: {error}"));
        assert_eq!(second_segment.mfa_code.as_deref(), Some("654321"));
        assert!(
            serde_json::from_value::<LoginByEmailCodeInput>(serde_json::json!({
                "email": "alice@example.com"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LoginByEmailCodeInput>(serde_json::json!({
                "email": "alice@example.com",
                "email_code": "123456",
                "user_id": 99
            }))
            .is_err()
        );
    }

    #[test]
    fn unified_invalid_code_error_matches_engine_shape() {
        match invalid_email_code() {
            BaseError::ParamInvalid(field, message) => {
                assert_eq!(field, "email_code");
                assert_eq!(message, "邮箱验证码无效或已过期");
            }
            other => panic!("统一错误必须是 ParamInvalid(email_code): {other:?}"),
        }
    }
}
