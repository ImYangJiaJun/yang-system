//! 校验账号密码并签发 Token。

use crate::addon::account::domain::policy::normalize_username;
use crate::addon::account::email_delivery::NewDeviceEmailSenderHandle;
use crate::addon::account::Account;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{
    normalize_email, AuthOperation, BrowserSession, CredentialVerifier, LoginAction, LoginInput,
    VerifiedSubject,
};
use yang_base::action::{ActionContext, ApiResponse, TypedHandler};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::token::TokenType;
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
        // 标识归一化后按是否含 @ 分派：邮箱走 email 唯一约束查询，其余按用户名。
        let identifier = input.username.trim().to_ascii_lowercase();
        let normalized = if identifier.contains('@') {
            normalize_email(&identifier)?
        } else {
            normalize_username(&identifier)?
        };
        // 分派查询：邮箱或用户名命中同一凭据投影。
        let user = if identifier.contains('@') {
            self.account
                .users()
                .find_credentials_by_email(ctx, &normalized)
                .await?
        } else {
            self.account
                .users()
                .find_credentials_by_username(ctx, &normalized)
                .await?
        };
        // 统一限流身份：同一账号无论用用户名还是邮箱登录都共享同一预算（以用户 ID
        // 为键），防止攻击者经邮箱维度绕过用户名维度的限流；用户不存在时按归一化标识键控。
        let limit_key = user
            .as_ref()
            .map(|user| user.id.to_string())
            .unwrap_or(normalized);
        self.account
            .rate_limiter()
            .check(ctx, AuthOperation::Login, &limit_key)
            .await?;
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
        // E-1a/E-1d：账号启用 TOTP 时登录为两段式——密码校验通过后，
        // 缺少第二因子码返回 SecondFactorRequired（前端据此弹出验证码输入框）；
        // 码错误按参数错误拒绝并计一次失败。暴露 MFA 状态以第一因子通过为前提，
        // 「用户不存在/密码错误」仍统一为 InvalidPassword（防枚举边界不变）。
        let totp_state = self
            .account
            .users()
            .find_totp_state_by_id(ctx, user.id)
            .await?;
        if let Some(state) = totp_state {
            if state.totp_activated_at.is_some() {
                let secret = self
                    .account
                    .users()
                    .decrypt_totp_secret(ctx, &state)
                    .await?;
                let mfa_code = input
                    .extra
                    .get("mfa_code")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                match mfa_code {
                    Some(code) => {
                        let accepted = self
                            .account
                            .verify_second_factor(ctx, user.id, &state, &secret, &code, true)
                            .await
                            .is_ok();
                        if !accepted {
                            self.account
                                .rate_limiter()
                                .record_failure(ctx, AuthOperation::Login, &limit_key)
                                .await?;
                            return Err(BaseError::ParamInvalid(
                                "mfa_code".to_string(),
                                "双重验证码错误或已过期".to_string(),
                            ));
                        }
                    }
                    None => {
                        // 两段式协议步骤而非攻击信号：不计失败，
                        // 避免正常登录的首阶段消耗失败额度。
                        return Err(BaseError::SecondFactorRequired);
                    }
                }
            }
        }
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
    // upsert 失败只记日志，不阻塞登录（会话可见性为 best-effort 增强）。
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
        tracing::warn!(error = %error, "登录会话持久化失败");
    }
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
                user_agent: user_agent.to_string(),
                result: "failed",
                failure_reason: Some(failure_reason),
            },
        )
        .await
}

/// 从新签发的 access/refresh token 提取 session_id 与两个 jti 并写入 `user_session`。
///
/// `pub(super)`：邮箱验证码免密登录（`login_by_email_code`）的成功路径
/// 与密码登录共用同一会话落库/成功事件/新设备提醒逻辑。
pub(super) async fn record_login_session(
    ctx: &ActionContext,
    account: &Account,
    access_token: &str,
    refresh_token: &str,
    ip: &str,
    user_agent: &str,
) -> Result<(), BaseError> {
    let claims = ctx.tools().token()?.verify_token(access_token)?;
    if claims.token_type != TokenType::Access {
        return Ok(()); // 防御性：只处理 access token
    }
    let Some(session_id) = claims
        .custom
        .get("session_id")
        .and_then(|value| value.as_str())
    else {
        return Ok(()); // 老逻辑无 session_id（登录 always 生成，不应发生）
    };
    let session_id = session_id.to_string();
    let user_id = claims
        .sub
        .parse::<i64>()
        .map_err(|_| BaseError::Unauthorized("Token subject 无效".to_string()))?;
    // refresh token 的 jti 独立于 access jti（框架各自生成）；逐台撤销必须同时拉黑
    // 二者，否则被踢设备仍可凭 refresh cookie 轮换续期。这里从 refresh token 解出 jti。
    let refresh_jti = ctx
        .tools()
        .token()?
        .verify_token(refresh_token)
        .ok()
        .map(|refresh_claims| refresh_claims.jti);
    let now = current_unix_timestamp()?;
    account
        .sessions()
        .upsert(
            ctx,
            crate::addon::account::domain::session::NewSession {
                session_id,
                user_id,
                jti: claims.jti,
                refresh_jti,
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
    // 新设备判断（路线图 C-3）：本次 upsert 后若用户活跃会话数恰为 1，
    // 说明这是该用户第一台设备（无历史会话指纹）→ best-effort 邮件提醒。
    let active_count = account
        .sessions()
        .active_count_for_user(ctx, user_id)
        .await
        .unwrap_or(0);
    if active_count <= 1 {
        if let Err(error) = notify_new_device(ctx, account, user_id, ip, user_agent).await {
            tracing::warn!(error = %error, "新设备登录提醒投递失败");
        }
    }
    Ok(())
}

/// 新设备登录成功提醒（best-effort，不阻塞登录路径）。
async fn notify_new_device(
    ctx: &ActionContext,
    account: &Account,
    user_id: i64,
    ip: &str,
    user_agent: &str,
) -> Result<(), BaseError> {
    let sender = ctx.tools().extension::<NewDeviceEmailSenderHandle>()?;
    let Some(record) = account.users().find_by_id(ctx, user_id).await? else {
        return Ok(());
    };
    let Some(email) = record.optional::<String>(crate::addon::account::user::table::EMAIL)? else {
        return Ok(()); // 无邮箱（如已匿名化）不提醒
    };
    let now = current_unix_timestamp()?;
    sender
        .send_new_device_login(&email, ip, user_agent, now)
        .await
        .map_err(|error| BaseError::Unknown(format!("新设备提醒投递失败: {error}")))
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
