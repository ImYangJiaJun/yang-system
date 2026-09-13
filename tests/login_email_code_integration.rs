//! 邮箱验证码免密登录（`[email.login]` 第四套独立验证码 key 域）的真实依赖集成测试。
//!
//! 覆盖完整链路（发码 → 用码登录 → me）、防枚举统一响应、验证码单次消费、
//! 错误尝试上限销毁、与注册验证码 key 域隔离、停用账号抑制投递与拒绝登录、
//! 与密码登录共享 AuthOperation::Login 限流预算，以及两段式 MFA（多因子任选
//! 登录阶段 1）：已激活 TOTP 的账号第一段只验不消费、第二因子三选一中
//! 备用邮箱通道严格禁用（同类不构成双因子）。
//! 需要 `YANG_SYSTEM_TEST_DATABASE_URL`（库名以 `_test` 结尾）与
//! `YANG_SYSTEM_TEST_REDIS_URL`（强制 DB 15）。

use anyhow::{ensure, Context};
use async_trait::async_trait;
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::auth::TotpLiteVerifier;
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::addon::account::email_delivery::{
    EmailDeliveryError, LoginEmailCodeSenderHandle, RegistrationEmailSender,
    RegistrationEmailSenderHandle, VerificationCodeSender, VerificationCodeSenderHandle,
};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::{
    EmailVerificationSettings, LoginEmailVerificationConfig, MfaEmailVerificationConfig,
    SecuritySettings, TotpSettings,
};
use yang_system::schema::sync_with_database;

const PASSWORD: &str = "correct-horse-battery-staple";
/// 统一无效验证码错误（防枚举）：任何消费失败都映射为这一个形状。
const INVALID_CODE_MESSAGE: &str = "邮箱验证码无效或已过期";
/// 测试 TOTP AEAD 密钥域（32+ 字节，与验证码密钥域互不相同）。
const TOTP_AEAD_KEY: &str = "integration-totp-aead-key-0123456789abcdef";

/// 同时捕获注册验证码与免密登录验证码的测试投递器（两类缓冲互相隔离）。
#[derive(Clone, Default)]
struct CapturingEmailSender {
    registration_codes: Arc<Mutex<BTreeMap<String, String>>>,
    login_codes: Arc<Mutex<BTreeMap<String, String>>>,
}

impl CapturingEmailSender {
    fn take_registration_code(&self, email: &str) -> anyhow::Result<String> {
        self.registration_codes
            .lock()
            .map_err(|_| anyhow::anyhow!("测试邮件缓冲区锁已损坏"))?
            .remove(email)
            .context("测试投递器未收到注册验证码")
    }

    fn take_login_code(&self, email: &str) -> anyhow::Result<Option<String>> {
        self.login_codes
            .lock()
            .map_err(|_| anyhow::anyhow!("测试邮件缓冲区锁已损坏"))
            .map(|mut codes| codes.remove(email))
    }
}

#[async_trait]
impl RegistrationEmailSender for CapturingEmailSender {
    async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        _expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.registration_codes
            .lock()
            .map_err(|_| EmailDeliveryError::Unavailable)?
            .insert(recipient.to_string(), code.to_string());
        Ok(())
    }
}

#[async_trait]
impl VerificationCodeSender for CapturingEmailSender {
    async fn send_verification_code(
        &self,
        recipient: &str,
        code: &str,
        _expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.login_codes
            .lock()
            .map_err(|_| EmailDeliveryError::Unavailable)?
            .insert(recipient.to_string(), code.to_string());
        Ok(())
    }
}

fn database_config() -> DatabaseConfig {
    DatabaseConfig::default()
        .with_max_connections(16)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

fn redis_config() -> RedisConfig {
    RedisConfig::default()
        .with_max_connections(16)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

fn security_settings() -> Arc<SecuritySettings> {
    security_settings_with_login_budget(1_000)
}

/// 生成安全设置；`login_budget` 是 AuthOperation::Login 的单身份尝试预算。
fn security_settings_with_login_budget(login_budget: u64) -> Arc<SecuritySettings> {
    Arc::new(SecuritySettings {
        argon2_max_concurrency: 4,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 10_000,
        auth_rate_limit_username_attempts: login_budget,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: true,
        trusted_proxy_cidrs: Vec::new(),
        totp: Some(TotpSettings {
            aead_key: TOTP_AEAD_KEY.to_string(),
            digits: 6,
        }),
    })
}

fn email_settings(namespace: String, secret: &str) -> EmailVerificationSettings {
    EmailVerificationSettings {
        namespace,
        secret: secret.to_string(),
        ttl_seconds: 60,
        resend_cooldown_seconds: 1,
        max_attempts: 3,
        send_window_seconds: 60,
        send_ip_attempts: 1_000,
        send_email_attempts: 100,
        send_global_attempts: 10_000,
    }
}

fn token_manager() -> TokenManager {
    TokenManager::new_symmetric(
        "login-email-integration-token-secret-32",
        Algorithm::HS256,
        "login-email-integration".to_string(),
        "login-email-integration-api".to_string(),
        300,
        3600,
    )
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "login-email-integration-step-up-secret-32",
            "login-email-integration-step-up",
            "login-email-sensitive-actions",
        )
        .unwrap_or_else(|error| panic!("集成测试 Step-up manager 应有效: {error}")),
    )
}

async fn connect_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config()).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await?;
    let name = name.context("登录邮箱验证码测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行登录邮箱验证码测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "登录邮箱验证码测试 Redis 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(url, redis_config())
        .await
        .map_err(Into::into)
}

async fn reset_database(database: &Database) -> anyhow::Result<()> {
    // 测试库专用：循环 DROP 直到库为空（与 registration_email_integration 同策略）。
    for _ in 0..64 {
        let tables: Vec<String> = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT TABLE_NAME FROM information_schema.tables WHERE TABLE_SCHEMA = DATABASE()",
        )
        .fetch_all(database.pool())
        .await?
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
        if tables.is_empty() {
            return Ok(());
        }
        for table in &tables {
            let _ = sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
                .execute(database.pool())
                .await;
        }
    }
    anyhow::bail!("测试库清理未在 64 轮内收敛（存在环状外键？）")
}

async fn reset_redis(redis: &RedisClient) -> anyhow::Result<()> {
    let keys = redis.keys("*").await?;
    if !keys.is_empty() {
        redis.del(&keys).await?;
    }
    Ok(())
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("登录邮箱验证码测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("登录邮箱验证码测试 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

fn action_handle(
    app: &BuiltApp,
    action: &str,
) -> anyhow::Result<yang_base::definition::ActionHandle> {
    let reference = ActionRef::new(
        ModuleName::new("account.user")
            .map_err(|error| anyhow::anyhow!("ModuleName 无效: {error}"))?,
        ActionName::new(action).map_err(|error| anyhow::anyhow!("ActionName 无效: {error}"))?,
    );
    app.registry()
        .resolve(&reference)
        .with_context(|| format!("Action 未注册: {reference}"))
}

async fn dispatch(
    app: &BuiltApp,
    action: &str,
    body: Value,
    headers: &[(&str, &str)],
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], peer_port))),
    );
    app.dispatch_context(
        action_handle(app, action).map_err(|error| BaseError::ConfigError(error.to_string()))?,
        context,
    )
    .await
}

/// 装配真实依赖的完整应用（注册验证码 + 免密登录验证码双独立 key 域）。
async fn build_login_app(
    database: &Database,
    redis: &RedisClient,
    sender: &CapturingEmailSender,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<Arc<BuiltApp>> {
    sync_with_database(
        connect_database().await?,
        database_config(),
        Arc::clone(&security),
    )
    .await?;
    let namespace = format!(
        "login-email-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(Database::from_pool(
                database.pool().clone(),
                database_config(),
            )?)
            .cache(redis.clone())
            .token(token_manager())
            .extension(AuthorizationVersionCache::new(
                redis.clone(),
                namespace.clone(),
            )?)
            .extension(step_up_manager())
            .extension(RegistrationEmailSenderHandle::new(sender.clone()))
            .extension(LoginEmailCodeSenderHandle::new(sender.clone()))
            .extension(VerificationCodeSenderHandle::new(sender.clone()))
            .config(
                email_settings(namespace.clone(), "integration-registration-secret-32bytes")
                    .engine_config(),
            )
            .config(MfaEmailVerificationConfig(
                email_settings(namespace.clone(), "integration-mfa-email-secret-32bytes")
                    .mfa_engine_config(),
            ))
            .config(TotpSettings {
                aead_key: TOTP_AEAD_KEY.to_string(),
                digits: 6,
            })
            .config(LoginEmailVerificationConfig(
                email_settings(namespace, "integration-login-email-secret-32bytes")
                    .login_engine_config(),
            ))
            .build()?,
    );
    let application = build_app(tools, security)?;
    Ok(Arc::new(application.runtime))
}

/// 注册一个邮箱已验证的账号。
async fn register_user(
    app: &BuiltApp,
    sender: &CapturingEmailSender,
    username: &str,
    email: &str,
    peer_port: u16,
) -> anyhow::Result<()> {
    let response = dispatch(
        app,
        "request_registration_email",
        json!({ "email": email }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "注册验证码请求返回业务失败");
    let code = sender.take_registration_code(email)?;
    let registered = dispatch(
        app,
        "register",
        json!({
            "username": username,
            "password": PASSWORD,
            "email": email,
            "email_code": code,
        }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(registered.code == 0, "注册必须成功");
    Ok(())
}

/// 请求免密登录验证码，返回统一 accepted 响应载荷。
async fn request_login_code(app: &BuiltApp, email: &str, peer_port: u16) -> anyhow::Result<Value> {
    let response = dispatch(
        app,
        "request_login_email_code",
        json!({ "email": email }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "登录验证码请求必须返回业务成功");
    response.data.context("登录验证码请求缺少 data")
}

/// 用邮箱验证码登录。
async fn login_by_email_code(
    app: &BuiltApp,
    email: &str,
    code: &str,
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    dispatch(
        app,
        "login_by_email_code",
        json!({ "email": email, "email_code": code }),
        &[],
        peer_port,
    )
    .await
}

/// 用邮箱验证码登录（两段式第二段携带 `mfa_code`）。
async fn login_by_email_code_2fa(
    app: &BuiltApp,
    email: &str,
    code: &str,
    mfa_code: &str,
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    dispatch(
        app,
        "login_by_email_code",
        json!({ "email": email, "email_code": code, "mfa_code": mfa_code }),
        &[],
        peer_port,
    )
    .await
}

/// 密码登录；`mfa_code` 为 Some 时携带第二因子码。
async fn login_with_password(
    app: &BuiltApp,
    username: &str,
    mfa_code: Option<&str>,
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let body = match mfa_code {
        Some(code) => json!({
            "username": username,
            "password": PASSWORD,
            "extra": { "mfa_code": code },
        }),
        None => json!({ "username": username, "password": PASSWORD }),
    };
    dispatch(app, "login", body, &[], peer_port).await
}

fn now_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

/// 为账号完成 TOTP 激活，返回（密钥，恢复码组）。
async fn activate_totp(
    app: &BuiltApp,
    username: &str,
    peer_port: u16,
) -> anyhow::Result<(String, Vec<String>)> {
    let login_response = login_with_password(app, username, None, peer_port).await?;
    let token = access_token(&login_response)?;
    let authorization = format!("Bearer {token}");
    let setup = dispatch(
        app,
        "totp_setup",
        json!({}),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await?;
    let secret = setup
        .data
        .as_ref()
        .and_then(|data| data["secret"].as_str())
        .map(str::to_string)
        .context("TOTP setup 响应缺少密钥")?;
    let code = TotpLiteVerifier::default().generate(&secret, now_seconds()?);
    let activated = dispatch(
        app,
        "totp_activate",
        json!({ "secret": secret, "code": code }),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await?;
    let data = activated.data.context("TOTP 激活响应缺少 data")?;
    ensure!(data["totp_activated"] == true, "TOTP 激活必须成功");
    let recovery_codes = data["recovery_codes"]
        .as_array()
        .context("TOTP 激活响应缺少恢复码")?
        .iter()
        .filter_map(|code| code.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    ensure!(!recovery_codes.is_empty(), "恢复码组不得为空");
    Ok((secret, recovery_codes))
}

/// 统计账号的登录事件行数（成功与失败合计）。
async fn login_event_count(database: &Database, username: &str) -> anyhow::Result<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM login_event WHERE user_id = (SELECT user_id FROM users WHERE username = ?)",
    )
    .bind(username)
    .fetch_one(database.pool())
    .await?;
    Ok(count)
}

/// 断言结果为统一无效验证码错误（防枚举形状）。
fn assert_invalid_code(result: Result<ApiResponse, BaseError>) -> anyhow::Result<()> {
    match result {
        Err(BaseError::ParamInvalid(field, message))
            if field == "email_code" && message == INVALID_CODE_MESSAGE =>
        {
            Ok(())
        }
        Err(error) => anyhow::bail!("预期统一验证码拒绝，实际为: {error}"),
        Ok(response) => anyhow::bail!("无效验证码不得成功: code={}", response.code),
    }
}

/// 提取登录响应中的 access token。
fn access_token(response: &ApiResponse) -> anyhow::Result<String> {
    response
        .data
        .as_ref()
        .and_then(|data| data["access_token"].as_str())
        .map(str::to_string)
        .context("登录响应缺少 access_token")
}

/// 生成一枚必定与 `code` 不同的 6 位数字码。
fn wrong_code(code: &str) -> &'static str {
    if code == "000000" {
        "111111"
    } else {
        "000000"
    }
}

/// 直接把账号状态改为 disabled（绕过 admin Action 的权限与 Step-up 链路）。
async fn disable_user(database: &Database, username: &str) -> anyhow::Result<()> {
    let affected = sqlx::query("UPDATE users SET status = 'disabled' WHERE username = ?")
        .bind(username)
        .execute(database.pool())
        .await?
        .rows_affected();
    ensure!(affected == 1, "停用必须命中恰好一行: {username}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn login_by_email_code_full_flow_issues_working_access_token() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(&app, &sender, "code_login_user", "code.login@example.com", 44_201).await?;

        // 请求登录验证码：统一 accepted 响应且真实投递 6 位数字码。
        let accepted = request_login_code(&app, "code.login@example.com", 44_201).await?;
        ensure!(accepted["accepted"] == true, "响应必须 accepted");
        ensure!(
            accepted["expires_in"].as_u64() == Some(60)
                && accepted["resend_after"].as_u64() == Some(1),
            "响应必须携带引擎配置的有效期与冷却"
        );
        let email_code = sender
            .take_login_code("code.login@example.com")?
            .context("已注册启用账号必须收到登录验证码")?;
        ensure!(
            email_code.len() == 6 && email_code.bytes().all(|byte| byte.is_ascii_digit()),
            "登录验证码必须是 6 位数字"
        );

        // 用码登录成功：与密码登录同形状（access_token + data 载荷）。
        let logged_in = login_by_email_code(&app, "code.login@example.com", &email_code, 44_201).await?;
        ensure!(logged_in.code == 0, "验证码登录必须成功");
        let token = access_token(&logged_in)?;
        ensure!(!token.is_empty(), "access_token 不得为空");

        // 签发后会话落库 + 登录事件落库（与密码登录同路径）。
        let sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_session WHERE user_id = (SELECT user_id FROM users WHERE username = 'code_login_user')",
        )
        .fetch_one(control.pool())
        .await?;
        ensure!(sessions >= 1, "登录成功后必须落 user_session 会话行");
        let login_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM login_event WHERE user_id = (SELECT user_id FROM users WHERE username = 'code_login_user')",
        )
        .fetch_one(control.pool())
        .await?;
        ensure!(login_events >= 1, "登录成功后必须落 login_event 事件");

        // 用签发的 token 调 me 成功。
        let authorization = format!("Bearer {token}");
        let me = dispatch(
            &app,
            "me",
            json!({}),
            &[("authorization", authorization.as_str())],
            44_201,
        )
        .await?;
        ensure!(me.code == 0, "me 必须成功");
        let me_data = me.data.context("me 响应缺少 data")?;
        ensure!(
            me_data["username"] == "code_login_user",
            "me 必须返回登录用户本人"
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn request_login_email_code_is_non_enumerating_for_unregistered_email() -> anyhow::Result<()>
{
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(&app, &sender, "enum_existing", "enum.existing@example.com", 44_301).await?;

        // 已注册邮箱与未注册邮箱的请求响应必须完全一致（accepted 统一语义）。
        let registered = request_login_code(&app, "enum.existing@example.com", 44_301).await?;
        let unregistered = request_login_code(&app, "enum.ghost@example.com", 44_301).await?;
        ensure!(
            registered == unregistered,
            "未注册邮箱响应必须与已注册邮箱逐字段一致: registered={registered}, unregistered={unregistered}"
        );
        ensure!(
            sender
                .take_login_code("enum.existing@example.com")?
                .is_some(),
            "已注册启用账号必须真实投递"
        );
        ensure!(
            sender.take_login_code("enum.ghost@example.com")?.is_none(),
            "未注册邮箱不得真实投递"
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn login_email_code_is_single_use() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(
            &app,
            &sender,
            "single_use_user",
            "single.use@example.com",
            44_401,
        )
        .await?;
        request_login_code(&app, "single.use@example.com", 44_401).await?;
        let code = sender
            .take_login_code("single.use@example.com")?
            .context("必须收到登录验证码")?;

        let first = login_by_email_code(&app, "single.use@example.com", &code, 44_401).await?;
        access_token(&first)?;
        // 同一码第二次使用必须返回统一无效验证码错误（Redis 原子单次消费）。
        assert_invalid_code(
            login_by_email_code(&app, "single.use@example.com", &code, 44_401).await,
        )?;
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn login_email_code_attempts_exhaustion_destroys_code() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(
            &app,
            &sender,
            "attempts_user",
            "attempts.login@example.com",
            44_501,
        )
        .await?;
        request_login_code(&app, "attempts.login@example.com", 44_501).await?;
        let code = sender
            .take_login_code("attempts.login@example.com")?
            .context("必须收到登录验证码")?;

        // 连续 max_attempts（3）次错误码：每次都返回统一无效验证码错误。
        for _ in 0..3 {
            assert_invalid_code(
                login_by_email_code(
                    &app,
                    "attempts.login@example.com",
                    wrong_code(&code),
                    44_501,
                )
                .await,
            )?;
        }
        // 错误尝试达上限后码已销毁：即使输入正确码也返回同一个统一错误。
        assert_invalid_code(
            login_by_email_code(&app, "attempts.login@example.com", &code, 44_501).await,
        )?;
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn registration_and_login_email_code_key_domains_are_isolated() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        // 方向一：注册域签发的验证码不能用于 login-by-email-code。
        let fresh_email = "domain.fresh@example.com";
        let registration_request = dispatch(
            &app,
            "request_registration_email",
            json!({ "email": fresh_email }),
            &[],
            44_601,
        )
        .await?;
        ensure!(registration_request.code == 0, "注册验证码请求必须成功");
        let registration_code = sender.take_registration_code(fresh_email)?;
        assert_invalid_code(
            login_by_email_code(&app, fresh_email, &registration_code, 44_601).await,
        )?;
        // 注册码未被 login 域消费：同邮箱仍可完成注册。
        let registered = dispatch(
            &app,
            "register",
            json!({
                "username": "domain_fresh_user",
                "password": PASSWORD,
                "email": fresh_email,
                "email_code": registration_code,
            }),
            &[],
            44_601,
        )
        .await?;
        ensure!(registered.code == 0, "注册码不得被 login 域消费");

        // 方向二：login 域签发的验证码不能用于注册（register 的 email_code）。
        register_user(
            &app,
            &sender,
            "domain_login_user",
            "domain.login@example.com",
            44_601,
        )
        .await?;
        request_login_code(&app, "domain.login@example.com", 44_601).await?;
        let login_code = sender
            .take_login_code("domain.login@example.com")?
            .context("必须收到登录验证码")?;
        assert_invalid_code(
            dispatch(
                &app,
                "register",
                json!({
                    "username": "domain_reverse_user",
                    "password": PASSWORD,
                    "email": "domain.login@example.com",
                    "email_code": login_code,
                }),
                &[],
                44_601,
            )
            .await,
        )?;
        // login 码未被注册域消费：同码仍可完成免密登录。
        let logged_in =
            login_by_email_code(&app, "domain.login@example.com", &login_code, 44_601).await?;
        access_token(&logged_in)?;
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn disabled_account_suppresses_delivery_and_rejects_code_login() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        // 先停用再发码：响应与启用账号统一（accepted），但不真实投递。
        register_user(
            &app,
            &sender,
            "disabled_first",
            "disabled.first@example.com",
            44_701,
        )
        .await?;
        disable_user(&control, "disabled_first").await?;
        let suppressed = request_login_code(&app, "disabled.first@example.com", 44_701).await?;
        ensure!(
            suppressed["accepted"] == true,
            "停用账号也必须返回统一 accepted"
        );
        ensure!(
            sender
                .take_login_code("disabled.first@example.com")?
                .is_none(),
            "停用账号不得真实投递"
        );

        // 先发码再停用：已送达的码在登录时仍返回统一无效验证码错误。
        register_user(
            &app,
            &sender,
            "disabled_after",
            "disabled.after@example.com",
            44_701,
        )
        .await?;
        request_login_code(&app, "disabled.after@example.com", 44_701).await?;
        let code = sender
            .take_login_code("disabled.after@example.com")?
            .context("启用时必须真实投递")?;
        disable_user(&control, "disabled_after").await?;
        assert_invalid_code(
            login_by_email_code(&app, "disabled.after@example.com", &code, 44_701).await,
        )?;
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn login_email_code_shares_login_rate_limit_budget() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        // 极小的单身份 Login 预算（3 次/窗口），IP 预算保持高位隔离变量。
        let app = build_login_app(
            &control,
            &redis,
            &sender,
            security_settings_with_login_budget(3),
        )
        .await?;

        let email = "rate.login@example.com";
        // 连续错误码登录：尝试计数用尽后必须返回 RateLimitExceeded。
        let mut limited = None;
        for attempt in 0..6_u32 {
            match login_by_email_code(&app, email, "000000", 44_801).await {
                Err(BaseError::RateLimitExceeded {
                    retry_after_seconds,
                }) => {
                    ensure!(retry_after_seconds >= 1, "限流必须携带 Retry-After");
                    limited = Some(attempt);
                    break;
                }
                Err(BaseError::ParamInvalid(field, _)) if field == "email_code" => {}
                other => anyhow::bail!("错误码登录只能是统一无效验证码或限流，实际: {other:?}"),
            }
        }
        let limited_at = limited.context("错误码登录必须在预算内触发 RateLimitExceeded")?;
        ensure!(
            limited_at <= 4,
            "限流触发不得晚于预算+1 次尝试: {limited_at}"
        );

        // 发码端点与密码登录共享同一 AuthOperation::Login 预算：同一身份同样被限流。
        match dispatch(
            &app,
            "request_login_email_code",
            json!({ "email": email }),
            &[],
            44_801,
        )
        .await
        {
            Err(BaseError::RateLimitExceeded { .. }) => {}
            other => anyhow::bail!("发码端点必须共享 Login 限流预算，实际: {other:?}"),
        }
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn totp_account_email_code_login_requires_second_factor() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(
            &app,
            &sender,
            "mfa_code_user",
            "mfa.code@example.com",
            44_901,
        )
        .await?;
        let (secret, _recovery) = activate_totp(&app, "mfa_code_user", 44_901).await?;
        // 激活会按秒粒度撤销既有 Token；跨过撤销水位线再登录，避免同秒新 Token 被误撤。
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        request_login_code(&app, "mfa.code@example.com", 44_901).await?;
        let email_code = sender
            .take_login_code("mfa.code@example.com")?
            .context("必须收到登录验证码")?;
        // 激活流程内的密码登录已落过事件，以此为基线观察第一段是否新增。
        let baseline_events = login_event_count(&control, "mfa_code_user").await?;

        // 第一段：验证码有效但缺第二因子 → SecondFactorRequired，不签发凭据。
        match login_by_email_code(&app, "mfa.code@example.com", &email_code, 44_901).await {
            Err(BaseError::SecondFactorRequired) => {}
            other => anyhow::bail!("缺第二因子必须返回 SecondFactorRequired，实际: {other:?}"),
        }
        // 协议步骤不是失败事件：第一段不得落 login_event（对齐密码登录）。
        ensure!(
            login_event_count(&control, "mfa_code_user").await? == baseline_events,
            "第一段 SecondFactorRequired 不得记录登录事件"
        );

        // 第一段只验不消费：TTL 内同一验证码可完成第二段（TOTP 动态码）。
        let totp_code = TotpLiteVerifier::default().generate(&secret, now_seconds()?);
        let logged_in = login_by_email_code_2fa(
            &app,
            "mfa.code@example.com",
            &email_code,
            &totp_code,
            44_901,
        )
        .await?;
        let token = access_token(&logged_in)?;
        ensure!(
            login_event_count(&control, "mfa_code_user").await? > baseline_events,
            "第二段成功必须记录登录事件"
        );

        // 签发的 token 可用；且邮箱验证码已被原子消费，重放必拒。
        let authorization = format!("Bearer {token}");
        let me = dispatch(
            &app,
            "me",
            json!({}),
            &[("authorization", authorization.as_str())],
            44_901,
        )
        .await?;
        ensure!(me.code == 0, "me 必须成功");
        let fresh_totp = TotpLiteVerifier::default().generate(&secret, now_seconds()?);
        assert_invalid_code(
            login_by_email_code_2fa(
                &app,
                "mfa.code@example.com",
                &email_code,
                &fresh_totp,
                44_901,
            )
            .await,
        )?;
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn backup_email_code_rejected_when_first_factor_is_email_code() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(&app, &sender, "backup_user", "backup@example.com", 45_001).await?;
        activate_totp(&app, "backup_user", 45_001).await?;
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // 取一枚登录验证码（第一因子）与一枚 MFA 备用邮箱验证码（[email.mfa] 域）。
        request_login_code(&app, "backup@example.com", 45_001).await?;
        let email_code = sender
            .take_login_code("backup@example.com")?
            .context("必须收到登录验证码")?;
        let mfa_request = dispatch(
            &app,
            "request_mfa_email_code",
            json!({ "username": "backup_user", "password": PASSWORD }),
            &[],
            45_001,
        )
        .await?;
        ensure!(mfa_request.code == 0, "MFA 备用邮箱验证码请求必须成功");
        // 备用邮箱验证码与登录验证码共用投递器的同一捕获缓冲（按邮箱键隔离先后取用）。
        let backup_code = sender
            .take_login_code("backup@example.com")?
            .context("必须收到 MFA 备用邮箱验证码")?;

        // 第一段只验不消费 → SecondFactorRequired。
        match login_by_email_code(&app, "backup@example.com", &email_code, 45_001).await {
            Err(BaseError::SecondFactorRequired) => {}
            other => anyhow::bail!("缺第二因子必须返回 SecondFactorRequired，实际: {other:?}"),
        }
        // 第二段以备用邮箱验证码作第二因子：同类因子不构成双因子，必须拒绝。
        match login_by_email_code_2fa(
            &app,
            "backup@example.com",
            &email_code,
            &backup_code,
            45_001,
        )
        .await
        {
            Err(BaseError::ParamInvalid(field, _)) if field == "mfa_code" => {}
            other => anyhow::bail!("备用邮箱验证码作第二因子必须拒绝，实际: {other:?}"),
        }
        // 备用通道未被触碰：该备用邮箱验证码未被消费，仍可在密码登录路径使用
        //（密码登录的备用邮箱通道保留，allow_backup_email = true）。
        match login_with_password(&app, "backup_user", None, 45_001).await {
            Err(BaseError::SecondFactorRequired) => {}
            other => {
                anyhow::bail!("密码登录缺第二因子必须返回 SecondFactorRequired，实际: {other:?}")
            }
        }
        let password_login =
            login_with_password(&app, "backup_user", Some(&backup_code), 45_001).await?;
        access_token(&password_login)?;
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn recovery_code_works_as_second_factor_on_email_code_login() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_login_app(&control, &redis, &sender, security_settings()).await?;

        register_user(
            &app,
            &sender,
            "recovery_user",
            "recovery@example.com",
            45_101,
        )
        .await?;
        let (_secret, recovery_codes) = activate_totp(&app, "recovery_user", 45_101).await?;
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        let recovery_code = recovery_codes
            .first()
            .context("激活必须签发恢复码")?
            .clone();

        request_login_code(&app, "recovery@example.com", 45_101).await?;
        let email_code = sender
            .take_login_code("recovery@example.com")?
            .context("必须收到登录验证码")?;

        // 第一段 → SecondFactorRequired；第二段用恢复码完成登录。
        match login_by_email_code(&app, "recovery@example.com", &email_code, 45_101).await {
            Err(BaseError::SecondFactorRequired) => {}
            other => anyhow::bail!("缺第二因子必须返回 SecondFactorRequired，实际: {other:?}"),
        }
        let logged_in = login_by_email_code_2fa(
            &app,
            "recovery@example.com",
            &email_code,
            &recovery_code,
            45_101,
        )
        .await?;
        access_token(&logged_in)?;

        // 恢复码单次消费：同一恢复码在密码登录路径重放必拒。
        match login_with_password(&app, "recovery_user", Some(&recovery_code), 45_101).await {
            Err(BaseError::ParamInvalid(field, _)) if field == "mfa_code" => {}
            other => anyhow::bail!("已消费的恢复码重放必须拒绝，实际: {other:?}"),
        }
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
