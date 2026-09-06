//! 校验账号密码并签发 Token。

use crate::addon::account::domain::policy::normalize_username;
use crate::addon::account::Account;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, AuthOperation, BrowserSession, CredentialVerifier, LoginAction, LoginInput,
    VerifiedSubject,
};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::transport::client_ip::client_ip_identity;
use yang_base::BaseError;
use yang_base::token::TokenType;

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
        // 标识归一化后按是否含 @ 分派：邮箱走 email 唯一约束查询，其余按用户名。
        // 两种标识统一归一化后落入同一查找函数族，限流键沿用归一化标识，
        // 防止攻击者经邮箱维度绕过用户名维度的限流与枚举防护。
        let identifier = input.username.trim().to_ascii_lowercase();
        let limit_key = if identifier.contains('@') {
            normalize_email(&identifier)?
        } else {
            normalize_username(&identifier)?
        };
        self.account
            .rate_limiter()
            .check(ctx, AuthOperation::Login, &limit_key)
            .await?;
        // 分派查询：邮箱或用户名命中同一凭据投影（限流键已用归一化标识）。
        let user = if identifier.contains('@') {
            self.account
                .users()
                .find_credentials_by_email(ctx, &limit_key)
                .await?
        } else {
            self.account
                .users()
                .find_credentials_by_username(ctx, &limit_key)
                .await?
        };
        // 等时校验：用户不存在时也必须执行一次完整的 Argon2 校验。
        // 若 miss 分支跳过哈希直接返回错误，「用户不存在」与「密码错误」的响应时间
        // 会相差一次 Argon2 运算（几十到几百毫秒），攻击者可据此枚举用户名是否存在。
        // 因此把 Option<密码哈希> 交给框架等时端口：None 时对内置 dummy 哈希走同一条
        // 校验代码路径，两条分支耗时分布一致，且对外返回同一个 InvalidPassword 错误。
        let password_matches = self
            .account
            .passwords()
            .verify_or_dummy(
                &input.password,
                user.as_ref().map(|user| user.password_hash.as_str()),
            )
            .await?;
        if !password_matches {
            return Err(BaseError::InvalidPassword);
        }
        // verify_or_dummy 对 None 恒返回 false，能走到这里说明用户一定存在。
        let user = user.ok_or(BaseError::InvalidPassword)?;
        Account::ensure_active(user.status)?;
        let claims = self.account.claims_for(ctx, user.id).await?;
        Ok(VerifiedSubject::new(user.id.to_string()).with_token_pair_claims(claims))
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
            if let Err(record_error) = record_login_failure(
                &record_ctx,
                &account,
                &input.username,
                &session_ip,
                &session_user_agent,
            )
            .await
            {
                tracing::warn!(error = %record_error, "登录失败事件记录失败");
            }
            return Err(error);
        }
    };
    // 登录成功：解析 access claims 中的 session_id/jti 并落一条会话行；
    // upsert 失败只记日志，不阻塞登录（会话可见性为 best-effort 增强）。
    if let Err(error) = record_login_session(
        &record_ctx,
        &account,
        &tokens.access_token,
        &session_ip,
        &session_user_agent,
    )
    .await
    {
        tracing::warn!(error = %error, "登录会话持久化失败");
    }
    Account::browser_session().token_response(tokens.access_token, tokens.refresh_token, secure)
}

/// 记录一次失败的登录尝试（best-effort；失败原因粗粒度）。
async fn record_login_failure(
    ctx: &ActionContext,
    account: &Account,
    identifier: &str,
    ip: &str,
    user_agent: &str,
) -> Result<(), BaseError> {
    let now = current_unix_timestamp()?;
    // 用户不存在/停用/密码错误统一粗粒度归类；不泄露具体原因给事件面。
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
    account
        .login_events()
        .append(
            ctx,
            crate::addon::account::domain::login_event::NewLoginEvent {
                user_id,
                occurred_at: now,
                ip: ip.to_string(),
                user_agent: user_agent.to_string(),
                result: "failed",
                failure_reason: Some("invalid_password"),
            },
        )
        .await
}

/// 从新签发的 access token 提取 session_id/jti 并写入 `user_session` 表。
async fn record_login_session(
    ctx: &ActionContext,
    account: &Account,
    access_token: &str,
    ip: &str,
    user_agent: &str,
) -> Result<(), BaseError> {
    let claims = ctx.tools().token()?.verify_token(access_token)?;
    if claims.token_type != TokenType::Access {
        return Ok(()); // 防御性：只处理 access token
    }
    let Some(session_id) = claims.custom.get("session_id").and_then(|value| value.as_str()) else {
        return Ok(()); // 老逻辑无 session_id（登录 always 生成，不应发生）
    };
    let session_id = session_id.to_string();
    let user_id = claims
        .sub
        .parse::<i64>()
        .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
    let now = current_unix_timestamp()?;
    account
        .sessions()
        .upsert(
            ctx,
            crate::addon::account::domain::session::NewSession {
                session_id,
                user_id,
                jti: claims.jti,
                ip: ip.to_string(),
                user_agent: user_agent.to_string(),
                now,
            },
        )
        .await?;
    // 成功事件与会话行同路径落库；失败不阻塞（best-effort）。
    let _ = account
        .login_events()
        .append(
            ctx,
            crate::addon::account::domain::login_event::NewLoginEvent {
                user_id: Some(user_id),
                occurred_at: now,
                ip: ip.to_string(),
                user_agent: user_agent.to_string(),
                result: "succeeded",
                failure_reason: None,
            },
        )
        .await;
    Ok(())
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