//! 用户查找（account.users.lookup）集成测试：authenticated-only、
//! 关键词命中（username/email 包含匹配）与分页上限。

use anyhow::{ensure, Context};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::addon::account::email_delivery::{
    EmailDeliveryError, RegistrationEmailSender, RegistrationEmailSenderHandle,
};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::{EmailVerificationSettings, SecuritySettings};
use yang_system::schema::sync_with_database;

const PASSWORD: &str = "correct-horse-battery-staple";

#[derive(Clone, Default)]
struct CapturingEmailSender {
    codes: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
}

#[async_trait]
impl RegistrationEmailSender for CapturingEmailSender {
    async fn send_registration_code(
        &self,
        recipient: &str,
        code: &str,
        _expires_in_seconds: u64,
    ) -> Result<(), EmailDeliveryError> {
        self.codes
            .lock()
            .map_err(|_| EmailDeliveryError::Unavailable)?
            .insert(recipient.to_owned(), code.to_owned());
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
        totp: None,
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
        "user-lookup-integration-token-secret",
        jsonwebtoken::Algorithm::HS256,
        "user-lookup-integration".to_string(),
        "user-lookup-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
}

async fn connect_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config()).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await?;
    let name = name.context("用户查找测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行用户查找测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "用户查找测试 Redis 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(url, redis_config())
        .await
        .map_err(Into::into)
}

async fn reset_database(database: &Database) -> anyhow::Result<()> {
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
        (Ok(()), Err(error), _) => Err(error.context("用户查找数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("用户查找 Redis 清理失败")),
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
    request: Request,
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], peer_port))),
    );
    let handle =
        action_handle(app, action).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    app.dispatch_context(handle, context).await
}

/// 注册邮箱已验证账号并密码登录，返回 access token。
async fn register_and_login(
    app: &BuiltApp,
    sender: &CapturingEmailSender,
    username: &str,
    email: &str,
    peer_port: u16,
) -> anyhow::Result<String> {
    let response = dispatch(
        app,
        "request_registration_email",
        Request::new(json!({ "email": email })),
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "注册验证码请求返回业务失败");
    let code = sender
        .codes
        .lock()
        .map_err(|_| anyhow::anyhow!("测试邮件缓冲区锁已损坏"))?
        .remove(&email.trim().to_ascii_lowercase())
        .context("测试投递器未收到注册验证码")?;
    let registered = dispatch(
        app,
        "register",
        Request::new(json!({
            "username": username,
            "password": PASSWORD,
            "email": email,
            "email_code": code,
        })),
        peer_port,
    )
    .await?;
    ensure!(registered.code == 0, "注册必须成功");
    let logged_in = dispatch(
        app,
        "login",
        Request::new(json!({ "username": username, "password": PASSWORD })),
        peer_port,
    )
    .await?;
    let token = logged_in
        .data
        .as_ref()
        .and_then(|data| data["access_token"].as_str())
        .map(str::to_string)
        .context("登录响应缺少 access_token")?;
    Ok(token)
}

/// 以真实 HTTP 形态调用 lookup：query 参数走 `Request::queries`、
/// 认证走 authorization header（无 token 时验证 authenticated-only）。
async fn lookup(
    app: &BuiltApp,
    token: Option<&str>,
    query: &[(&str, &str)],
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(json!({}));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let query = HashMap::from_iter(
        query
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string())),
    );
    dispatch(app, "lookup", request.queries(query), peer_port).await
}

fn usernames(payload: &Value) -> Vec<&str> {
    payload
        .get("users")
        .and_then(Value::as_array)
        .map(|users| {
            users
                .iter()
                .filter_map(|user| user["username"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn user_lookup_is_authenticated_only_and_filters_by_keyword() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        sync_with_database(
            connect_database().await?,
            database_config(),
            security_settings(),
        )
        .await?;

        let namespace = format!(
            "user-lookup-{}",
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
                .extension(RegistrationEmailSenderHandle::new(sender.clone()))
                .config(email_settings(namespace).engine_config())
                .build()?,
        );
        let application = build_app(Arc::clone(&tools), security_settings())?;
        let app = Arc::new(application.runtime);

        // 两个可搜索用户（username 与 email 均可命中）。
        let token_a =
            register_and_login(&app, &sender, "alpha_user", "alpha@example.com", 44_001).await?;
        let _ = register_and_login(&app, &sender, "beta_user", "beta@example.com", 44_002).await?;

        // 1) authenticated-only：未登录 → 401（Unauthorized）。
        match lookup(&app, None, &[], 44_003).await {
            Err(BaseError::Unauthorized(message)) => ensure!(
                message.contains("Bearer"),
                "未登录必须提示缺少 Token: {message}"
            ),
            other => anyhow::bail!("未登录查找必须被拒: {other:?}"),
        }

        // 2) 登录后空关键词 → 正常分页，两个用户都在。
        let all = lookup(&app, Some(&token_a), &[("q", "")], 44_003).await?;
        ensure!(all.code == 0, "空关键词查找应成功");
        let data = all.data.context("查找响应缺少 data")?;
        ensure!(
            usernames(&data) == ["alpha_user", "beta_user"],
            "空关键词应返回全量用户: {:?}",
            usernames(&data)
        );

        // 3) 关键词按 username 包含匹配。
        let hit = lookup(&app, Some(&token_a), &[("q", "alpha")], 44_003).await?;
        ensure!(hit.code == 0, "关键词命中查找应成功");
        let data = hit.data.context("查找响应缺少 data")?;
        ensure!(
            usernames(&data) == ["alpha_user"],
            "关键词 alpha 应只命中 alpha_user: {:?}",
            usernames(&data)
        );

        // 4) 关键词按 email 包含匹配（不要求完整邮箱）。
        let hit_email = lookup(&app, Some(&token_a), &[("q", "beta@")], 44_003).await?;
        ensure!(hit_email.code == 0, "邮箱关键词查找应成功");
        let data = hit_email.data.context("查找响应缺少 data")?;
        ensure!(
            usernames(&data) == ["beta_user"],
            "关键词 beta@ 应命中 beta_user 的邮箱: {:?}",
            usernames(&data)
        );

        // 5) 响应契约：每个用户只暴露 id/username/email/status。
        for user in data["users"]
            .as_array()
            .context("查找响应 users 必须是数组")?
        {
            let keys: Vec<&str> = user
                .as_object()
                .map(|map| map.keys().map(String::as_str).collect())
                .unwrap_or_default();
            ensure!(
                keys == ["email", "id", "status", "username"],
                "查找响应字段超出契约: {keys:?}"
            );
        }

        // 6) 关键词里的 LIKE 通配符按字面匹配：`alpha%` 不命中任何用户名。
        let literal = lookup(&app, Some(&token_a), &[("q", "alpha%")], 44_003).await?;
        ensure!(literal.code == 0, "字面关键词查找应成功");
        let data = literal.data.context("查找响应缺少 data")?;
        ensure!(
            usernames(&data).is_empty(),
            "转义后的通配符不得按模式解释: {:?}",
            usernames(&data)
        );

        // 7) page_size 超限（>50）→ ParamInvalid（防 DoS）。
        match lookup(&app, Some(&token_a), &[("page_size", "51")], 44_003).await {
            Err(BaseError::ParamInvalid(field, _)) if field == "page_size" => {}
            other => anyhow::bail!("page_size 超限必须被拒: {other:?}"),
        }
        // 上限内（50）与默认分页均可正常查询。
        let capped = lookup(&app, Some(&token_a), &[("page_size", "50")], 44_003).await?;
        ensure!(capped.code == 0, "page_size=50 应在上限内");
        let defaulted = lookup(&app, Some(&token_a), &[], 44_003).await?;
        ensure!(defaulted.code == 0, "缺省分页应可用");

        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
