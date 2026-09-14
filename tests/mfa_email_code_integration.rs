//! 登录 MFA 备用邮箱验证码与 TOTP 停用的真实依赖集成测试。
//!
//! 覆盖对抗边界（等时密码校验防枚举、未激活账号不真实投递、邮箱码单次
//! 消费）与停用链路（Step-up 重认证、恢复码作废、回到单因子登录）。
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
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
    VerificationCodeSender, VerificationCodeSenderHandle,
};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::{
    EmailVerificationSettings, MfaEmailVerificationConfig, SecuritySettings, TotpSettings,
};
use yang_system::schema::sync_with_database;

const PASSWORD: &str = "correct-horse-battery-staple";
/// 测试 TOTP AEAD 密钥域（32+ 字节，与验证码密钥域互不相同）。
const TOTP_AEAD_KEY: &str = "integration-totp-aead-key-0123456789abcdef";

/// 同时捕获注册验证码与 MFA 登录验证码的测试投递器（两类缓冲互相隔离）。
#[derive(Clone, Default)]
struct CapturingEmailSender {
    registration_codes: Arc<Mutex<BTreeMap<String, String>>>,
    mfa_codes: Arc<Mutex<BTreeMap<String, String>>>,
}

impl CapturingEmailSender {
    fn take_registration_code(&self, email: &str) -> anyhow::Result<String> {
        self.registration_codes
            .lock()
            .map_err(|_| anyhow::anyhow!("测试邮件缓冲区锁已损坏"))?
            .remove(email)
            .context("测试投递器未收到注册验证码")
    }

    fn take_mfa_code(&self, email: &str) -> anyhow::Result<Option<String>> {
        self.mfa_codes
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
        self.mfa_codes
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
    Arc::new(SecuritySettings {
        argon2_max_concurrency: 4,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 10_000,
        auth_rate_limit_username_attempts: 1_000,
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
        "mfa-email-integration-token-secret-32",
        Algorithm::HS256,
        "mfa-email-integration".to_string(),
        "mfa-email-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "mfa-email-integration-step-up-secret-32",
            "mfa-email-integration-step-up",
            "mfa-email-sensitive-actions",
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
    let name = name.context("MFA 邮箱验证码测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行 MFA 邮箱验证码测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "MFA 邮箱验证码测试 Redis 必须使用独立 DB 15"
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

/// 装配真实依赖的完整应用（注册验证码 + MFA 邮箱验证码双独立 key 域 + TOTP）。
async fn build_mfa_app(
    database: &Database,
    redis: &RedisClient,
    sender: &CapturingEmailSender,
) -> anyhow::Result<Arc<BuiltApp>> {
    sync_with_database(
        connect_database().await?,
        database_config(),
        security_settings(),
    )
    .await?;
    let namespace = format!(
        "mfa-email-{}",
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
            .extension(VerificationCodeSenderHandle::new(sender.clone()))
            .config(
                email_settings(namespace.clone(), "integration-registration-secret-32bytes")
                    .engine_config(),
            )
            .config(MfaEmailVerificationConfig(
                email_settings(namespace, "integration-mfa-email-secret-32bytes")
                    .mfa_engine_config(),
            ))
            .config(TotpSettings {
                aead_key: TOTP_AEAD_KEY.to_string(),
                digits: 6,
            })
            .build()?,
    );
    let application = build_app(tools, security_settings())?;
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

/// 密码登录；`mfa_code` 为 Some 时携带第二因子码。
async fn login(
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

/// 提取登录响应中的 access token。
fn access_token(response: &ApiResponse) -> anyhow::Result<String> {
    response
        .data
        .as_ref()
        .and_then(|data| data["access_token"].as_str())
        .map(str::to_string)
        .context("登录响应缺少 access_token")
}

/// 为账号完成 TOTP 激活，返回（密钥，恢复码组）。
///
/// totp_setup 与 totp_activate 现均受 Step-up 保护（会话劫持者不得擅自绑定
/// 认证器并夺走恢复码），因此每个调用前都要先触发 challenge 并完成密码重认证。
async fn activate_totp(
    app: &BuiltApp,
    username: &str,
    peer_port: u16,
) -> anyhow::Result<(String, Vec<String>)> {
    let login_response = login(app, username, None, peer_port).await?;
    let token = access_token(&login_response)?;
    let authorization = format!("Bearer {token}");

    // totp_setup：先触发 challenge，完成密码重认证，携带 proof 重试。
    let setup_challenge = match dispatch(
        app,
        "totp_setup",
        json!({}),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        other => anyhow::bail!("totp_setup 缺少 proof 必须返回 Step-up challenge，实际: {other:?}"),
    };
    let setup_proof = complete_step_up(app, username, peer_port, &setup_challenge).await?;
    let setup = dispatch(
        app,
        "totp_setup",
        json!({}),
        &[
            ("authorization", authorization.as_str()),
            ("x-step-up-proof", setup_proof.as_str()),
        ],
        peer_port,
    )
    .await?;
    let secret = setup
        .data
        .as_ref()
        .and_then(|data| data["secret"].as_str())
        .map(str::to_string)
        .context("TOTP setup 响应缺少密钥")?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let code = TotpLiteVerifier::default().generate(&secret, now);

    // totp_activate：同样先触发 challenge 再重认证。
    let activate_challenge = match dispatch(
        app,
        "totp_activate",
        json!({ "secret": secret, "code": code }),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        other => {
            anyhow::bail!("totp_activate 缺少 proof 必须返回 Step-up challenge，实际: {other:?}")
        }
    };
    let activate_proof = complete_step_up(app, username, peer_port, &activate_challenge).await?;
    let activated = dispatch(
        app,
        "totp_activate",
        json!({ "secret": secret, "code": code }),
        &[
            ("authorization", authorization.as_str()),
            ("x-step-up-proof", activate_proof.as_str()),
        ],
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

/// 完成一次 Step-up（密码重认证），返回一次性 proof。
async fn complete_step_up(
    app: &BuiltApp,
    username: &str,
    peer_port: u16,
    challenge: &str,
) -> anyhow::Result<String> {
    let completed = dispatch(
        app,
        "step_up_complete",
        json!({
            "challenge": challenge,
            "credentials": { "username": username, "password": PASSWORD },
        }),
        &[],
        peer_port,
    )
    .await?;
    completed
        .data
        .as_ref()
        .and_then(|data| data["proof"].as_str())
        .map(str::to_string)
        .context("Step-up 完成响应缺少 proof")
}

fn now_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn mfa_email_code_is_bounded_non_enumerating_and_single_use() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_mfa_app(&control, &redis, &sender).await?;

        // 两个账号：totp_user 激活 TOTP，plain_user 不激活。
        register_user(&app, &sender, "totp_user", "totp@example.com", 44_001).await?;
        register_user(&app, &sender, "plain_user", "plain@example.com", 44_001).await?;
        activate_totp(&app, "totp_user", 44_001).await?;
        // 激活会按秒粒度撤销既有 Token；跨过撤销水位线再登录，避免同秒新 Token 被误撤。
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // 密码错误：与登录一致的统一 InvalidPassword，不投递。
        match dispatch(
            &app,
            "request_mfa_email_code",
            json!({ "username": "totp_user", "password": "wrong-password" }),
            &[],
            44_001,
        )
        .await
        {
            Err(BaseError::InvalidPassword) => {}
            other => anyhow::bail!("密码错误必须返回统一 InvalidPassword，实际: {other:?}"),
        }
        // 不存在的用户：同样统一 InvalidPassword（等时校验防枚举）。
        match dispatch(
            &app,
            "request_mfa_email_code",
            json!({ "username": "ghost_user", "password": PASSWORD }),
            &[],
            44_001,
        )
        .await
        {
            Err(BaseError::InvalidPassword) => {}
            other => anyhow::bail!("不存在用户必须返回统一 InvalidPassword，实际: {other:?}"),
        }
        ensure!(
            sender.take_mfa_code("totp@example.com")?.is_none(),
            "密码校验未通过不得投递 MFA 验证码"
        );

        // 密码正确但未激活 TOTP 的账号：统一 accepted 响应但不真实投递（防枚举）。
        let suppressed = dispatch(
            &app,
            "request_mfa_email_code",
            json!({ "username": "plain_user", "password": PASSWORD }),
            &[],
            44_001,
        )
        .await?;
        ensure!(
            suppressed.code == 0
                && suppressed
                    .data
                    .as_ref()
                    .and_then(|data| data["accepted"].as_bool())
                    == Some(true),
            "未激活 TOTP 的账号也必须返回统一 accepted 响应"
        );
        ensure!(
            sender.take_mfa_code("plain@example.com")?.is_none(),
            "未激活 TOTP 的账号不得真实投递"
        );

        // 已激活 TOTP 的账号：真实投递 6 位数字码。
        let accepted = dispatch(
            &app,
            "request_mfa_email_code",
            json!({ "username": "totp_user", "password": PASSWORD }),
            &[],
            44_001,
        )
        .await?;
        ensure!(accepted.code == 0, "MFA 邮箱验证码请求必须成功");
        let email_code = sender
            .take_mfa_code("totp@example.com")?
            .context("已激活 TOTP 的账号必须收到 MFA 验证码")?;
        ensure!(
            email_code.len() == 6 && email_code.bytes().all(|byte| byte.is_ascii_digit()),
            "MFA 邮箱验证码必须是 6 位数字"
        );

        // 两段式登录：无码 → SecondFactorRequired；邮箱码 → 登录成功。
        match login(&app, "totp_user", None, 44_001).await {
            Err(BaseError::SecondFactorRequired) => {}
            other => anyhow::bail!("缺少第二因子必须返回 SecondFactorRequired，实际: {other:?}"),
        }
        let logged_in = login(&app, "totp_user", Some(&email_code), 44_001).await?;
        access_token(&logged_in)?;

        // 单次消费：同一邮箱码重放必须失败（统一 mfa_code 参数错误）。
        match login(&app, "totp_user", Some(&email_code), 44_001).await {
            Err(BaseError::ParamInvalid(field, _)) if field == "mfa_code" => {}
            other => anyhow::bail!("已消费的邮箱码重放必须拒绝，实际: {other:?}"),
        }
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("MFA 测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("MFA 测试 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn totp_deactivate_requires_step_up_and_restores_single_factor() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_mfa_app(&control, &redis, &sender).await?;

        register_user(&app, &sender, "mfa_admin", "mfa.admin@example.com", 44_101).await?;
        let (secret, _recovery_codes) = activate_totp(&app, "mfa_admin", 44_101).await?;
        // 激活会按秒粒度撤销既有 Token；跨过撤销水位线再登录，避免同秒新 Token 被误撤。
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // 激活后旧会话已失效：用 TOTP 码重新登录拿到新 token。
        let totp_code = TotpLiteVerifier::default().generate(&secret, now_seconds()?);
        let login_response = login(&app, "mfa_admin", Some(&totp_code), 44_101).await?;
        let token = access_token(&login_response)?;
        let authorization = format!("Bearer {token}");

        // 无 proof 直接停用 → Step-up challenge。
        let challenge = match dispatch(
            &app,
            "totp_deactivate",
            json!({}),
            &[("authorization", authorization.as_str())],
            44_101,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => challenge,
            other => anyhow::bail!("缺少 proof 必须返回 Step-up challenge，实际: {other:?}"),
        };

        // 完成重认证：密码 + 当前 TOTP 动态码（已激活账号 Step-up 必须双因子）。
        let mfa_code = TotpLiteVerifier::default().generate(&secret, now_seconds()?);
        let completed = dispatch(
            &app,
            "step_up_complete",
            json!({
                "challenge": challenge.challenge,
                "credentials": {
                    "username": "mfa_admin",
                    "password": PASSWORD,
                    "mfa_code": mfa_code,
                },
            }),
            &[],
            44_101,
        )
        .await?;
        let proof = completed
            .data
            .as_ref()
            .and_then(|data| data["proof"].as_str())
            .map(str::to_string)
            .context("Step-up 完成响应缺少 proof")?;

        // 携带 proof 停用成功：响应确认关闭且要求重新登录。
        let deactivated = dispatch(
            &app,
            "totp_deactivate",
            json!({}),
            &[
                ("authorization", authorization.as_str()),
                ("x-step-up-proof", proof.as_str()),
            ],
            44_101,
        )
        .await?;
        let data = deactivated.data.context("TOTP 停用响应缺少 data")?;
        ensure!(data["totp_activated"] == false, "停用响应必须确认关闭");
        ensure!(data["relogin_required"] == true, "停用后必须要求重新登录");

        // 存储面：密钥/激活时间/恢复码全部置空。
        let stored: (Option<String>, Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT totp_secret, totp_activated_at, totp_recovery_digest FROM users WHERE username = 'mfa_admin'",
        )
        .fetch_one(control.pool())
        .await?;
        ensure!(
            stored.0.is_none() && stored.1.is_none() && stored.2.is_none(),
            "停用后 TOTP 密钥、激活时间与恢复码摘要必须全部置空"
        );

        // 恢复码随停用全部作废，登录回到单因子。
        ensure!(
            login(&app, "mfa_admin", None, 44_101).await.is_ok(),
            "停用后仅凭密码必须能登录"
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("TOTP 停用测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("TOTP 停用测试 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn totp_code_cannot_be_replayed_within_same_window() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_mfa_app(&control, &redis, &sender).await?;
        register_user(
            &app,
            &sender,
            "totp_replay_user",
            "totp.replay@example.com",
            44_201,
        )
        .await?;
        let (secret, _recovery_codes) = activate_totp(&app, "totp_replay_user", 44_201).await?;
        // 激活会按秒粒度撤销既有 Token；跨过撤销水位线再登录。
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        let code = TotpLiteVerifier::default().generate(&secret, now_seconds()?);
        // 首次登录成功。
        let first = login(&app, "totp_replay_user", Some(&code), 44_201).await?;
        ensure!(first.code == 0, "首次 TOTP 登录必须成功: {}", first.message);

        // 同一码在同一 30 秒窗口内重放：必须被拒绝（防重放）。
        let replay = login(&app, "totp_replay_user", Some(&code), 44_201).await;
        let replayed_ok = match replay {
            Ok(response) => response.code == 0,
            Err(_) => false,
        };
        ensure!(
            !replayed_ok,
            "同一 TOTP 码在同一窗口内重放必须被拒绝，实际成功"
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("TOTP 重放测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("TOTP 重放测试 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}
