//! 校验账号密码并签发 Token。

use crate::addon::account::domain::policy::normalize_username;
use crate::addon::account::Account;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, BrowserSession, CredentialVerifier, LoginAction, LoginInput, VerifiedSubject,
};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::transport::client_ip::client_ip_identity;
use yang_base::BaseError;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct BrowserLoginInput {
    /// 用户名、邮箱或其他登录标识。
    username: String,
    /// 登录凭据。
    password: String,
    /// 可选业务扩展字段。
    #[serde(default)]
    extra: serde_json::Value,
}

impl ParamInput for BrowserLoginInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 把账号凭据校验接到框架内置 `LoginAction` 的端口。
#[derive(Clone)]
struct UserCredentialVerifier {
    account: Arc<Account>,
}

#[async_trait]
impl CredentialVerifier for UserCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let mfa_code = input
            .extra
            .get("mfa_code")
            .and_then(serde_json::Value::as_str);
        let user_id = self
            .account
            .verify_password_credentials(
                ctx,
                &input.username,
                &input.password,
                mfa_code,
                true,
                "login",
            )
            .await?;
        let claims = self.account.claims_for(ctx, user_id).await?;
        Ok(VerifiedSubject::new(user_id.to_string()).with_token_pair_claims(claims))
    }
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: BrowserLoginInput,
    account: Arc<Account>,
) -> Result<ApiResponse, BaseError> {
    let secure = BrowserSession::validate_same_origin(&ctx.request)?;
    // 会话持久化所需信息在 ctx 被 move 进 LoginAction 前提取；
    // `ActionContext` 现可 Clone（框架支持），record_ctx 供签发后落会话行。
    let session_ip = client_ip_identity(&ctx).into_owned();
    let session_user_agent = ctx
        .request
        .get_header("user-agent")
        .unwrap_or_default()
        .to_string();
    let record_ctx = ctx.clone();
    let login_result = LoginAction::new(UserCredentialVerifier {
        account: Arc::clone(&account),
    })
    .handle(
        ctx,
        LoginInput {
            username: input.username.clone(),
            password: input.password,
            extra: input.extra,
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
                    &input.username,
                    &session_ip,
                    &session_user_agent,
                    failure_reason,
                )
                .await
                {
                    tracing::warn!(error = %record_error, "登录失败事件记录失败");
                }
            }
            return Err(error);
        }
    };
    // 登录成功：解析 access claims 中的 session_id/jti 并落一条会话行；
    // 会话行是逐台撤销与 refresh fail-closed 的权威事实；落库失败必须让登录失败，
    // 不能发出一个无法撤销、也无法再次刷新的“半登录”令牌对。
    account
        .record_login_session(
            &record_ctx,
            &tokens.access_token,
            &tokens.refresh_token,
            &session_ip,
            &session_user_agent,
        )
        .await?;
    Account::browser_session().token_response(tokens.access_token, tokens.refresh_token, secure)
}

/// 记录一次失败的登录尝试（best-effort；失败原因粗粒度）。
///
/// `pub(super)`：登录 MFA 邮箱验证码端点（`request_mfa_email_code`）同样是在线
/// 密码校验入口，失败事件沿用同一记录路径。
pub(super) async fn record_login_failure(
    ctx: &ActionContext,
    account: &Account,
    identifier: &str,
    ip: &str,
    user_agent: &str,
    failure_reason: &'static str,
) -> Result<(), BaseError> {
    let now = current_unix_timestamp()?;
    let normalized = identifier.trim().to_ascii_lowercase();
    let user_id = if identifier.contains('@') {
        let email = normalize_email(&normalized)?;
        account
            .users()
            .find_credentials_by_email(ctx, &email)
            .await?
            .map(|user| user.id)
    } else {
        let username = normalize_username(&normalized)?;
        account
            .users()
            .find_credentials_by_username(ctx, &username)
            .await?
            .map(|user| user.id)
    };
    // 用户不存在（user_id 为 None）时归为 user_not_found，否则沿用调用方按错误类型
    // 给出的粗粒度原因（invalid_password/disabled/rate_limited）。
    let failure_reason = if user_id.is_none() {
        "user_not_found"
    } else {
        failure_reason
    };
    account
        .login_events()
        .append(
            ctx,
            crate::addon::account::domain::login_event::NewLoginEvent {
                user_id,
                occurred_at: now,
                ip: ip.to_string(),
                user_agent: truncate_user_agent(user_agent),
                result: "failed",
                failure_reason: Some(failure_reason),
            },
        )
        .await
}

/// 会话/登录事件表的 `user_agent` 列宽为 512；超长 UA 会让写入失败。
fn truncate_user_agent(value: &str) -> String {
    value.chars().take(512).collect()
}

fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("login"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&account))
        })
        .route(HttpMethod::Post, "/api/v1/users/login")
        .display_name("登录")
        .description("校验账号密码并签发 Token")
        .public()
        .register()
}

#[cfg(test)]
mod tests {
    use super::truncate_user_agent;

    #[test]
    fn 用户代理超长时按列宽截断且保持字符边界() {
        let long = "a".repeat(600);
        assert_eq!(truncate_user_agent(&long).chars().count(), 512);
        let multibyte = "中".repeat(600);
        assert_eq!(truncate_user_agent(&multibyte).chars().count(), 512);
        assert_eq!(truncate_user_agent("短"), "短");
    }
}
