//! 用户头像（`user_avatar` 运行支撑表）的真实依赖集成测试。
//!
//! 覆盖上传 → me 投影 → 读取回环、etag 版本变化、非法输入拒绝（超大 /
//! MIME 与 magic bytes 不符 / 非图片字节 / 白名单外 MIME）、delete_account
//! 匿名化事务内删除头像行，以及 get_avatar 的登录要求。
//! 需要 `YANG_SYSTEM_TEST_DATABASE_URL`（库名以 `_test` 结尾）与
//! `YANG_SYSTEM_TEST_REDIS_URL`（强制 DB 15）。

use anyhow::{ensure, Context};
use async_trait::async_trait;
use base64::Engine;
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
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
/// 解码后图片字节上限（与 src/addon/account/domain/avatar.rs 的 AVATAR_MAX_BYTES 对齐）。
const AVATAR_MAX_BYTES: usize = 40 * 1024;

/// 捕获注册验证码的测试投递器（注册链路需要真实发码）。
#[derive(Clone, Default)]
struct CapturingEmailSender {
    codes: Arc<Mutex<BTreeMap<String, String>>>,
}

impl CapturingEmailSender {
    fn take_code(&self, email: &str) -> anyhow::Result<String> {
        self.codes
            .lock()
            .map_err(|_| anyhow::anyhow!("测试邮件缓冲区锁已损坏"))?
            .remove(email)
            .context("测试投递器未收到注册验证码")
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
        "avatar-integration-token-secret-32-byte",
        Algorithm::HS256,
        "avatar-integration".to_string(),
        "avatar-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "avatar-integration-step-up-secret-32byte",
            "avatar-integration-step-up",
            "avatar-sensitive-actions",
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
    let name = name.context("头像测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行头像测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "头像测试 Redis 必须使用独立 DB 15"
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
        (Ok(()), Err(error), _) => Err(error.context("头像测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("头像测试 Redis 清理失败")),
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

/// 装配真实依赖的完整应用（注册验证码 + 声明式 Schema 同步出 user_avatar 表）。
async fn build_avatar_app(
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
        "avatar-{}",
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
            .config(email_settings(namespace).engine_config())
            .build()?,
    );
    let application = build_app(tools, security_settings())?;
    Ok(Arc::new(application.runtime))
}

/// 注册一个邮箱已验证的账号并密码登录，返回（access token, 用户 ID）。
async fn register_and_login(
    app: &BuiltApp,
    sender: &CapturingEmailSender,
    username: &str,
    email: &str,
    peer_port: u16,
) -> anyhow::Result<(String, i64)> {
    let response = dispatch(
        app,
        "request_registration_email",
        json!({ "email": email }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "注册验证码请求返回业务失败");
    let code = sender.take_code(email)?;
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
    let user_id = registered
        .data
        .as_ref()
        .and_then(|data| data["id"].as_i64())
        .context("注册响应缺少用户 ID")?;

    let logged_in = dispatch(
        app,
        "login",
        json!({ "username": username, "password": PASSWORD }),
        &[],
        peer_port,
    )
    .await?;
    let token = logged_in
        .data
        .as_ref()
        .and_then(|data| data["access_token"].as_str())
        .map(str::to_string)
        .context("登录响应缺少 access_token")?;
    Ok((token, user_id))
}

/// 上传头像。
async fn upload_avatar(
    app: &BuiltApp,
    token: &str,
    bytes: &[u8],
    mime: &str,
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let authorization = format!("Bearer {token}");
    dispatch(
        app,
        "upload_avatar",
        json!({
            "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mime": mime,
        }),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await
}

/// 读取指定用户头像。
///
/// `user_id` 是 query 来源参数（与浏览器 `?user_id=N` 同路径）：放进
/// `Request::queries`，不再放 body——守护 GET 无 body 的真实 HTTP 形态。
async fn get_avatar(
    app: &BuiltApp,
    token: &str,
    user_id: i64,
    peer_port: u16,
) -> Result<ApiResponse, BaseError> {
    let authorization = format!("Bearer {token}");
    let request = Request::new(json!({}))
        .queries(std::collections::HashMap::from([(
            "user_id".to_string(),
            user_id.to_string(),
        )]))
        .header("authorization", authorization.as_str());
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], peer_port))),
    );
    app.dispatch_context(
        action_handle(app, "get_avatar")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
        context,
    )
    .await
}

/// 读取当前用户的 me 视图。
async fn me(app: &BuiltApp, token: &str, peer_port: u16) -> anyhow::Result<Value> {
    let authorization = format!("Bearer {token}");
    let response = dispatch(
        app,
        "me",
        json!({}),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "me 必须成功");
    response.data.context("me 响应缺少 data")
}

/// 合法 PNG 样本（与 validate_avatar_image 单元测试同构的头字节序列）。
fn png_with_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&13_u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

/// 合法 GIF89a 样本。
fn gif_with_dimensions(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes
}

/// 合法 JPEG 样本（SOI + APP0 + SOF0）。
fn jpeg_with_dimensions(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
    bytes
}

/// 断言结果为 ParamInvalid，且字段名匹配。
fn assert_param_invalid(result: Result<ApiResponse, BaseError>, field: &str) -> anyhow::Result<()> {
    match result {
        Err(BaseError::ParamInvalid(actual, _)) if actual == field => Ok(()),
        Err(error) => anyhow::bail!("预期 ParamInvalid({field})，实际为: {error}"),
        Ok(response) => anyhow::bail!("非法输入不得成功: code={}", response.code),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn avatar_upload_me_projection_and_fetch_roundtrip() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_avatar_app(&control, &redis, &sender).await?;
        let (token, user_id) = register_and_login(
            &app,
            &sender,
            "avatar_user",
            "avatar.user@example.com",
            45_001,
        )
        .await?;

        // 上传前：me 的 avatar_version 为 null，get_avatar 两个字段都为 null。
        let before = me(&app, &token, 45_001).await?;
        ensure!(
            before.get("avatar_version") == Some(&Value::Null),
            "无头像时 me 的 avatar_version 必须为 null: {before}"
        );
        let empty = get_avatar(&app, &token, user_id, 45_001).await?;
        let empty_data = empty.data.context("get_avatar 响应缺少 data")?;
        ensure!(
            empty_data["etag"] == Value::Null && empty_data["data_url"] == Value::Null,
            "无头像时 etag 与 data_url 必须都为 null: {empty_data}"
        );

        // 上传 PNG：返回 avatar_version；me 投影同一版本；get_avatar 取回原始内容。
        let png = png_with_dimensions(64, 64);
        let uploaded = upload_avatar(&app, &token, &png, "image/png", 45_001).await?;
        let version = uploaded
            .data
            .as_ref()
            .and_then(|data| data["avatar_version"].as_str())
            .map(str::to_string)
            .context("上传响应缺少 avatar_version")?;
        ensure!(
            version.len() == 32,
            "avatar_version 必须是 32 位十六进制 etag"
        );
        let after = me(&app, &token, 45_001).await?;
        ensure!(
            after["avatar_version"] == Value::String(version.clone()),
            "me 的 avatar_version 必须等于上传返回的版本"
        );
        let fetched = get_avatar(&app, &token, user_id, 45_001).await?;
        let fetched_data = fetched.data.context("get_avatar 响应缺少 data")?;
        let expected_data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        ensure!(
            fetched_data["etag"] == Value::String(version.clone()),
            "get_avatar 的 etag 必须等于 avatar_version"
        );
        ensure!(
            fetched_data["data_url"] == Value::String(expected_data_url),
            "get_avatar 的 data_url 必须与上传内容一致"
        );

        // 再次上传不同内容：etag 变化，读取到新内容。
        let gif = gif_with_dimensions(32, 32);
        let reuploaded = upload_avatar(&app, &token, &gif, "image/gif", 45_001).await?;
        let new_version = reuploaded
            .data
            .as_ref()
            .and_then(|data| data["avatar_version"].as_str())
            .map(str::to_string)
            .context("二次上传响应缺少 avatar_version")?;
        ensure!(new_version != version, "内容变化后 etag 必须变化");
        let refreshed = get_avatar(&app, &token, user_id, 45_001).await?;
        let refreshed_data = refreshed.data.context("get_avatar 响应缺少 data")?;
        ensure!(
            refreshed_data["etag"] == Value::String(new_version),
            "get_avatar 必须返回新 etag"
        );
        ensure!(
            refreshed_data["data_url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/gif;base64,")),
            "get_avatar 必须返回新 MIME 的 data_url"
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
async fn avatar_upload_rejects_invalid_inputs() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_avatar_app(&control, &redis, &sender).await?;
        let (token, _user_id) =
            register_and_login(&app, &sender, "avatar_guard", "avatar.guard@example.com", 45_101)
                .await?;

        // 超过 40 KiB 解码上限。
        let oversized = vec![0_u8; AVATAR_MAX_BYTES + 1];
        assert_param_invalid(
            upload_avatar(&app, &token, &oversized, "image/png", 45_101).await,
            "content_base64",
        )?;
        // 声明 image/png 但内容是 JPEG 字节（magic bytes 不符）。
        assert_param_invalid(
            upload_avatar(&app, &token, &jpeg_with_dimensions(8, 8), "image/png", 45_101).await,
            "content_base64",
        )?;
        // 非图片字节。
        assert_param_invalid(
            upload_avatar(&app, &token, b"not an image at all", "image/png", 45_101).await,
            "content_base64",
        )?;
        // 白名单外 MIME。
        assert_param_invalid(
            upload_avatar(&app, &token, &png_with_dimensions(8, 8), "image/svg+xml", 45_101).await,
            "mime",
        )?;
        // 宽高超过 1024。
        assert_param_invalid(
            upload_avatar(&app, &token, &png_with_dimensions(1025, 8), "image/png", 45_101).await,
            "content_base64",
        )?;
        // 非法 base64。
        let authorization = format!("Bearer {token}");
        assert_param_invalid(
            dispatch(
                &app,
                "upload_avatar",
                json!({ "content_base64": "!!!not-base64!!!", "mime": "image/png" }),
                &[("authorization", authorization.as_str())],
                45_101,
            )
            .await,
            "content_base64",
        )?;
        // 全部拒绝后不得留下头像行。
        let stored: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_avatar WHERE user_id = (SELECT user_id FROM users WHERE username = 'avatar_guard')",
        )
        .fetch_one(control.pool())
        .await?;
        ensure!(stored == 0, "被拒绝的上传不得落库");
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn delete_account_removes_avatar_row() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_avatar_app(&control, &redis, &sender).await?;
        // 首账号会被引导 claimer 变成系统管理员，而 spec §8.2 禁止删除最后一名
        // 启用的系统管理员（Task 13）。先消费掉那个名额，owner 才是可删除的普通账号。
        register_and_login(
            &app,
            &sender,
            "avatar_bootstrap",
            "avatar.bootstrap@example.com",
            45_201,
        )
        .await?;
        let (owner_token, owner_id) = register_and_login(
            &app,
            &sender,
            "avatar_owner",
            "avatar.owner@example.com",
            45_201,
        )
        .await?;
        let (observer_token, _observer_id) = register_and_login(
            &app,
            &sender,
            "avatar_observer",
            "avatar.observer@example.com",
            45_201,
        )
        .await?;

        // owner 上传头像。
        let uploaded = upload_avatar(
            &app,
            &owner_token,
            &png_with_dimensions(48, 48),
            "image/png",
            45_201,
        )
        .await?;
        ensure!(uploaded.code == 0, "上传必须成功");

        // delete_account 需要 Step-up：先取 challenge，再完成重认证拿 proof。
        let authorization = format!("Bearer {owner_token}");
        let delete_body = json!({ "confirmation": "delete my account" });
        let challenge = match dispatch(
            &app,
            "delete_account",
            delete_body.clone(),
            &[("authorization", authorization.as_str())],
            45_201,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => challenge,
            other => anyhow::bail!("缺少 proof 必须返回 Step-up challenge，实际: {other:?}"),
        };
        let completed = dispatch(
            &app,
            "step_up_complete",
            json!({
                "challenge": challenge.challenge,
                "credentials": { "username": "avatar_owner", "password": PASSWORD },
            }),
            &[],
            45_201,
        )
        .await?;
        let proof = completed
            .data
            .as_ref()
            .and_then(|data| data["proof"].as_str())
            .map(str::to_string)
            .context("Step-up 完成响应缺少 proof")?;
        let deleted = dispatch(
            &app,
            "delete_account",
            delete_body,
            &[
                ("authorization", authorization.as_str()),
                ("x-step-up-proof", proof.as_str()),
            ],
            45_201,
        )
        .await?;
        ensure!(deleted.code == 0, "delete_account 必须成功");

        // 头像行已随匿名化事务删除：存储面与读取面双重断言。
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_avatar WHERE user_id = ?")
            .bind(owner_id)
            .fetch_one(control.pool())
            .await?;
        ensure!(stored == 0, "delete_account 必须删除头像行");
        let fetched = get_avatar(&app, &observer_token, owner_id, 45_201).await?;
        let fetched_data = fetched.data.context("get_avatar 响应缺少 data")?;
        ensure!(
            fetched_data["etag"] == Value::Null && fetched_data["data_url"] == Value::Null,
            "删除账号后 get_avatar 必须返回 null 字段: {fetched_data}"
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
async fn get_avatar_requires_authentication() -> anyhow::Result<()> {
    let control = connect_database().await?;
    let redis = connect_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let sender = CapturingEmailSender::default();
        let app = build_avatar_app(&control, &redis, &sender).await?;
        // 无 token 调用 get_avatar 必须 Unauthorized。
        match dispatch(&app, "get_avatar", json!({ "user_id": 1 }), &[], 45_301).await {
            Err(BaseError::Unauthorized(_)) => {}
            other => anyhow::bail!("未登录读取头像必须返回 Unauthorized，实际: {other:?}"),
        }
        // upload_avatar 同样要求登录。
        match dispatch(
            &app,
            "upload_avatar",
            json!({ "content_base64": "aGVsbG8=", "mime": "image/png" }),
            &[],
            45_301,
        )
        .await
        {
            Err(BaseError::Unauthorized(_)) => {}
            other => anyhow::bail!("未登录上传头像必须返回 Unauthorized，实际: {other:?}"),
        }
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
