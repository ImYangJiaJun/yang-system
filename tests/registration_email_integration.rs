use anyhow::{ensure, Context};
use async_trait::async_trait;
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::{EmailVerificationSettings, SecuritySettings};
use yang_system::migrations::{execute_with_database, MigrationCommand};
use yang_system::modules::account::email_delivery::{
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
};

const PASSWORD: &str = "correct-horse-battery-staple";

#[derive(Clone, Default)]
struct CapturingEmailSender {
    codes: Arc<Mutex<BTreeMap<String, String>>>,
    fail: Arc<AtomicBool>,
}

impl CapturingEmailSender {
    fn take_code(&self, email: &str) -> anyhow::Result<Option<String>> {
        self.codes
            .lock()
            .map_err(|_| anyhow::anyhow!("测试邮件缓冲区锁已损坏"))
            .map(|mut codes| codes.remove(email))
    }

    fn fail_next(&self, fail: bool) {
        self.fail.store(fail, Ordering::Release);
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
        if self.fail.load(Ordering::Acquire) {
            return Err(EmailDeliveryError::Unavailable);
        }
        self.codes
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
    })
}

fn email_settings(namespace: String) -> EmailVerificationSettings {
    EmailVerificationSettings {
        namespace,
        secret: "integration-email-verification-secret-32-bytes-minimum".to_string(),
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
        "registration-email-integration-token-secret",
        Algorithm::HS256,
        "registration-email-integration".to_string(),
        "registration-email-integration-api".to_string(),
        300,
        3600,
    )
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "registration-email-step-up-secret-32-bytes",
            "registration-email-step-up",
            "registration-email-sensitive-actions",
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
    let name = name.context("邮箱验证码测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行邮箱验证码测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "邮箱验证码测试 Redis 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(url, redis_config())
        .await
        .map_err(Into::into)
}

async fn reset_database(database: &Database) -> anyhow::Result<()> {
    for table in [
        "password_reset_token",
        "audit_event",
        "authorization_outbox",
        "work_task",
        "work_project",
        "org_user",
        "org_org",
        "admin_user",
        "users",
        "_migrations",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(database.pool())
            .await?;
    }
    Ok(())
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
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let context = app.context(Request::new(body)).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], peer_port))),
    );
    app.dispatch_context(
        action_handle(app, action).map_err(|error| BaseError::ConfigError(error.to_string()))?,
        context,
    )
    .await
}

async fn request_code(
    app: &BuiltApp,
    sender: &CapturingEmailSender,
    email: &str,
    peer_port: u16,
) -> anyhow::Result<String> {
    let response = dispatch(
        app,
        "request_registration_email",
        json!({ "email": email }),
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "验证码请求返回业务失败");
    let data = response.data.context("验证码请求缺少 data")?;
    ensure!(data["accepted"] == true, "验证码请求必须使用通用接收语义");
    ensure!(
        data.get("code").is_none() && !data.to_string().contains(email),
        "响应不得包含验证码或邮箱原文"
    );
    sender
        .take_code(&email.trim().to_ascii_lowercase())?
        .context("测试投递器未收到验证码")
}

fn registration_body(username: &str, email: &str, code: &str) -> Value {
    json!({
        "username": username,
        "password": PASSWORD,
        "email": email,
        "email_code": code,
    })
}

fn assert_invalid_code(result: Result<ApiResponse, BaseError>) -> anyhow::Result<()> {
    match result {
        Err(BaseError::ParamInvalid(field, message))
            if field == "email_code" && message == "邮箱验证码无效或已过期" =>
        {
            Ok(())
        }
        Err(error) => anyhow::bail!("预期统一验证码拒绝，实际为: {error}"),
        Ok(response) => anyhow::bail!("无效验证码不得成功: code={}", response.code),
    }
}

fn wrong_code(code: &str) -> &'static str {
    if code == "000000" {
        "111111"
    } else {
        "000000"
    }
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("邮箱验证码数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("邮箱验证码 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn registration_email_code_is_private_bounded_and_single_use() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        execute_with_database(
            MigrationCommand::Apply,
            connect_database().await?,
            database_config(),
            security_settings(),
        )
        .await?;

        let namespace = format!(
            "registration-email-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let sender = CapturingEmailSender::default();
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(Database::from_pool(
                    control.pool().clone(),
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
                .config(email_settings(namespace))
                .build()?,
        );
        let application = build_app(Arc::clone(&tools), security_settings())?;
        let app = Arc::new(application.runtime);

        let email = " Alice.Verify@Example.COM ";
        let normalized_email = "alice.verify@example.com";
        let code = request_code(&app, &sender, email, 43_001).await?;
        match dispatch(
            &app,
            "request_registration_email",
            json!({ "email": normalized_email }),
            43_001,
        )
        .await
        {
            Err(BaseError::RateLimitExceeded {
                retry_after_seconds,
            }) => ensure!(retry_after_seconds >= 1, "冷却拒绝必须携带 Retry-After"),
            other => anyhow::bail!("同邮箱立即重发必须被冷却，实际: {other:?}"),
        }
        let keys = redis.keys("*").await?;
        ensure!(
            keys.iter().all(|key| !key.contains(normalized_email)),
            "Redis key 不得暴露邮箱原文"
        );
        for key in &keys {
            if let Some(value) = redis.get(key).await? {
                ensure!(!value.contains(&code), "Redis value 不得保存验证码原文");
                ensure!(
                    !value.contains(normalized_email),
                    "Redis value 不得保存邮箱原文"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(1_100)).await;

        assert_invalid_code(
            dispatch(
                &app,
                "register",
                registration_body("wrong_once", normalized_email, wrong_code(&code)),
                43_001,
            )
            .await,
        )?;
        let registered = dispatch(
            &app,
            "register",
            registration_body("verified_user", normalized_email, &code),
            43_001,
        )
        .await?;
        ensure!(registered.code == 0, "正确验证码必须完成注册");
        let registered_data = registered.data.context("注册响应缺少 data")?;
        ensure!(
            registered_data["email"] == normalized_email,
            "注册响应必须返回规范化已验证邮箱"
        );
        let stored: (String, i64) = sqlx::query_as(
            "SELECT email, email_verified_at FROM users WHERE username = 'verified_user'",
        )
        .fetch_one(control.pool())
        .await?;
        ensure!(
            stored.0 == normalized_email && stored.1 > 0,
            "数据库必须保存验证事实"
        );
        ensure!(
            redis.keys("*:code:*").await?.is_empty(),
            "注册成功后验证码必须从 Redis 原子消费"
        );
        assert_invalid_code(
            dispatch(
                &app,
                "register",
                registration_body("replay_user", "replay@example.com", &code),
                43_001,
            )
            .await,
        )?;

        let suppressed = dispatch(
            &app,
            "request_registration_email",
            json!({ "email": normalized_email }),
            43_001,
        )
        .await?;
        ensure!(suppressed.code == 0, "已有邮箱请求仍返回通用接收响应");
        ensure!(
            sender.take_code(normalized_email)?.is_none(),
            "已有邮箱不得再次投递注册验证码"
        );
        assert_invalid_code(
            dispatch(
                &app,
                "register",
                registration_body("existing_email_probe", normalized_email, "000000"),
                43_001,
            )
            .await,
        )?;

        let concurrent_email = "concurrent@example.com";
        let concurrent_code = request_code(&app, &sender, concurrent_email, 43_002).await?;
        let mut tasks = Vec::new();
        for username in ["concurrent_one", "concurrent_two"] {
            let app = Arc::clone(&app);
            let body = registration_body(username, concurrent_email, &concurrent_code);
            tasks.push(tokio::spawn(async move {
                dispatch(&app, "register", body, 43_002).await
            }));
        }
        let mut successes = 0;
        let mut denied = 0;
        for task in tasks {
            match task.await.context("并发注册任务 panic")? {
                Ok(response) if response.code == 0 => successes += 1,
                Err(BaseError::ParamInvalid(field, _)) if field == "email_code" => denied += 1,
                other => anyhow::bail!("并发双消费返回非预期结果: {other:?}"),
            }
        }
        ensure!(
            successes == 1 && denied == 1,
            "同一验证码并发只能成功一次: successes={successes}, denied={denied}"
        );

        let attempts_email = "attempts@example.com";
        let attempts_code = request_code(&app, &sender, attempts_email, 43_003).await?;
        for index in 0..3 {
            assert_invalid_code(
                dispatch(
                    &app,
                    "register",
                    registration_body(
                        &format!("attempt_{index}"),
                        attempts_email,
                        wrong_code(&attempts_code),
                    ),
                    43_003,
                )
                .await,
            )?;
        }
        assert_invalid_code(
            dispatch(
                &app,
                "register",
                registration_body("attempt_exhausted", attempts_email, &attempts_code),
                43_003,
            )
            .await,
        )?;

        let expired_email = "expired@example.com";
        let expired_code = request_code(&app, &sender, expired_email, 43_004).await?;
        let code_keys = redis.keys("*:code:*").await?;
        ensure!(code_keys.len() == 1, "应仅有待过期的一枚验证码");
        ensure!(
            redis.expire(code_keys[0].clone(), 1).await?,
            "应可缩短测试 TTL"
        );
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert_invalid_code(
            dispatch(
                &app,
                "register",
                registration_body("expired_user", expired_email, &expired_code),
                43_004,
            )
            .await,
        )?;

        sender.fail_next(true);
        let before_failed_send = redis.keys("*:code:*").await?.len();
        match dispatch(
            &app,
            "request_registration_email",
            json!({ "email": "smtp-failure@example.com" }),
            43_005,
        )
        .await
        {
            Err(BaseError::HttpRequestFailed(message)) => {
                ensure!(message == "邮件服务暂不可用", "错误必须脱敏")
            }
            other => anyhow::bail!("SMTP 失败必须映射为可重试上游失败: {other:?}"),
        }
        ensure!(
            redis.keys("*:code:*").await?.len() == before_failed_send,
            "SMTP 失败不得留下可消费验证码"
        );
        sender.fail_next(false);

        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
