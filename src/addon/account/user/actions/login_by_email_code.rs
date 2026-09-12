//! 邮箱验证码免密登录（公开端点；验证码 Redis 原子单次消费，防枚举统一错误）。
//!
//! 复用框架 `LoginAction` 的签发路径：`LoginInput.username` 承载邮箱、
//! `LoginInput.password` 承载验证码，由 [`EmailCodeCredentialVerifier`] 完成
//! 限流、验证码消费、账号存在性与状态校验；签发后会话落库、登录事件与
//! 新设备邮件提醒与密码登录完全同路径（复用 login.rs 的共享函数）。

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
        // 验证码先消费（Redis 原子单次消费，错误尝试达上限即销毁）；
        // 失败计一次限流失败再返回统一无效验证码错误。
        if let Err(error) = verification.consume(ctx, &email, &input.password).await {
            self.account
                .rate_limiter()
                .record_failure(ctx, AuthOperation::Login, &email)
                .await?;
            return Err(error);
        }
        // 验证码已被消费：邮箱未注册或账号停用也返回同一个无效验证码错误，
        // 不泄露账号存在性与状态变化。
        let user = self
            .account
            .users()
            .find_credentials_by_email(ctx, &email)
            .await?
            .ok_or_else(invalid_email_code)?;
        if !user.status.is_active() {
            return Err(invalid_email_code());
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
            extra: serde_json::Value::Null,
        },
    )
    .await;
    // 失败路径 best-effort 记录登录事件（粗粒度原因，不记录明文）；
    // 成功路径由 record_login_session 在会话落库时一并记录。
    let tokens = match login_result {
        Ok(tokens) => tokens,
        Err(error) => {
            if let Err(record_error) = record_login_failure(
                &record_ctx,
                &account,
                &input.email,
                &session_ip,
                &session_user_agent,
            )
            .await
            {
                tracing::warn!(error = %record_error, "邮箱验证码登录失败事件记录失败");
            }
            return Err(error);
        }
    };
    if let Err(error) = record_login_session(
        &record_ctx,
        &account,
        &tokens.access_token,
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
        .description("校验邮箱一次性验证码并签发 Token（免密登录，验证码单次消费）")
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
        assert_eq!(names, ["email", "email_code"]);
        assert!(params.as_slice().iter().all(|param| param.required));
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
