//! 匿名化删除的对抗性集成测试：删除后凭据材料与 PII 必须被清理。
//!
//! 回归对象（评审 2026-09-14 高严重度）：`delete_account` 只改写 username/email/
//! status 并递增双版本，但保留 password_hash（Argon2 摘要）、totp_secret（AEAD
//! 密文，可逆）、totp_recovery_digest（恢复码摘要），且不删除 user_session 与
//! login_event 行（含 ip/user_agent 个人数据）。本测试验证删除后这些材料被清空。

mod common;

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::SecuritySettings;
use yang_system::schema::sync_with_database;

use common::{take_registration_code, RegistrationEmailToolsExt};

const PASSWORD: &str = "correct-horse-battery-staple";

fn database_config() -> DatabaseConfig {
    DatabaseConfig::default()
        .with_max_connections(20)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

fn redis_config() -> RedisConfig {
    RedisConfig::default()
        .with_max_connections(20)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

/// delete_account 依赖双版本失效传播，需开启凭据版本签发；同时进入 step_up_targets。
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

fn token_manager() -> TokenManager {
    TokenManager::new_symmetric_keyring(
        "account-deletion-active".to_string(),
        "account-deletion-secret-at-least-32-bytes",
        Vec::new(),
        Algorithm::HS256,
        "yang-system-account-deletion".to_string(),
        "yang-system-account-deletion-api".to_string(),
        3600,
        2_592_000,
    )
    .unwrap_or_else(|error| panic!("删除测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "account-deletion-step-up-secret-32-bytes",
            "yang-system-account-deletion-step-up",
            "yang-system-account-deletion-sensitive",
        )
        .unwrap_or_else(|error| panic!("删除测试 Step-up manager 应构建成功: {error}")),
    )
}

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接删除测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取删除测试数据库名失败")?;
    let name = name.context("删除测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行删除测试"
    );
    Ok(database)
}

async fn connect_test_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "删除测试 Redis URL 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, redis_config())
        .await
        .context("连接删除测试 Redis 失败")
}

async fn reset_database(database: &Database) -> anyhow::Result<()> {
    for table in [
        "user_avatar",
        "password_reset_token",
        "user_session",
        "login_event",
        "audit_event",
        "authorization_outbox",
        // 组与授权事实表：首账号引导会把注册者写进内置全权组，`user_group` 与
        // `system_owner` 因此对 `users` 持有 RESTRICT 外键；`authz_grant` 无外键，
        // 但按 `user_id` 计数时会串到下一轮。漏删它们时收尾 `DROP users` 会以
        // 3730 失败（`permission_groups_integration` 的夹具同因）。
        "user_group",
        "permission_group_item",
        "permission_group",
        "system_owner",
        "authz_grant",
        "users",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(database.pool())
            .await
            .with_context(|| format!("清理删除测试表失败: {table}"))?;
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
    module: &str,
    action: &str,
) -> anyhow::Result<yang_base::definition::ActionHandle> {
    let reference = ActionRef::new(
        ModuleName::new(module).map_err(|error| anyhow::anyhow!("ModuleName 无效: {error}"))?,
        ActionName::new(action).map_err(|error| anyhow::anyhow!("ActionName 无效: {error}"))?,
    );
    app.registry()
        .resolve(&reference)
        .with_context(|| format!("Action 未注册: {reference}"))
}

async fn dispatch(
    app: &BuiltApp,
    module: &str,
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
        action_handle(app, module, action)
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
        context,
    )
    .await
}

fn access_token(response: &ApiResponse) -> anyhow::Result<String> {
    response
        .data
        .as_ref()
        .and_then(|data| data["access_token"].as_str())
        .map(str::to_string)
        .context("登录响应缺少 access_token")
}

async fn register_and_login(
    app: &BuiltApp,
    suffix: u128,
    peer_port: u16,
) -> anyhow::Result<String> {
    let username = format!("delete_{suffix}");
    let email = format!("{username}@example.test");
    dispatch(
        app,
        "account.user",
        "request_registration_email",
        json!({ "email": email }),
        &[],
        peer_port,
    )
    .await?;
    let code = take_registration_code(&email)?;
    let registered = dispatch(
        app,
        "account.user",
        "register",
        json!({ "username": username, "password": PASSWORD, "email": email, "email_code": code }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(registered.code == 0, "注册必须成功: {}", registered.message);
    let login = dispatch(
        app,
        "account.user",
        "login",
        json!({ "username": username, "password": PASSWORD }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(login.code == 0, "登录必须成功: {}", login.message);
    access_token(&login)
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("删除测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("删除测试 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn delete_account_clears_credentials_sessions_and_login_events() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        sync_with_database(
            connect_test_database().await?,
            database_config(),
            security_settings(),
        )
        .await?;
        let deployment = format!(
            "account-deletion-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(Database::from_pool(
                    control.pool().clone(),
                    database_config(),
                )?)
                .cache(redis.clone())
                .with_registration_email(format!("email-{deployment}"))
                .extension(AuthorizationVersionCache::new(redis.clone(), deployment)?)
                .extension(step_up_manager())
                .token(token_manager())
                .build()?,
        );
        let app = build_app(tools, security_settings())?;
        let runtime = app.runtime;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let username = format!("delete_{suffix}");
        // 首账号会被引导 claimer 变成系统管理员，而 spec §8.2 禁止删除最后一名
        // 启用的系统管理员（Task 13）。先消费掉那个名额——否则本用例的被删账号
        // 一出生就是系统管理员，撞在新守卫上。
        register_and_login(&runtime, suffix - 1, 42_100).await?;
        let token = register_and_login(&runtime, suffix, 42_101).await?;

        // 记录删除前的用户 ID 与密码摘要（删除后 username 会被改写，后续按 ID 查询）。
        let (user_id, before_hash): (i64, String) =
            sqlx::query_as("SELECT id, password_hash FROM users WHERE username = ?")
                .bind(&username)
                .fetch_one(control.pool())
                .await?;
        // 直写可逆凭据材料模拟已存在状态（删除后必须被清空）。
        sqlx::query(
            "UPDATE users SET totp_secret = 'aead-ciphertext', totp_recovery_digest = '[]' WHERE id = ?",
        )
        .bind(user_id)
        .execute(control.pool())
        .await?;
        // 登录事件与会话行此时应已存在（登录成功即落库）。
        let session_count_before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_session WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(control.pool())
                .await?;
        ensure!(session_count_before >= 1, "删除前应存在会话行");

        // delete_account 受 Step-up 保护：先触发 challenge。
        let authorization = format!("Bearer {token}");
        let challenge = match dispatch(
            &runtime,
            "account.user",
            "delete_account",
            json!({ "confirmation": "delete my account" }),
            &[("authorization", authorization.as_str())],
            42_101,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
            other => anyhow::bail!("缺少 proof 必须返回 Step-up challenge，实际: {other:?}"),
        };
        let completed = dispatch(
            &runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": challenge,
                "credentials": { "username": username, "password": PASSWORD },
            }),
            &[],
            42_101,
        )
        .await?;
        let proof = completed
            .data
            .as_ref()
            .and_then(|data| data["proof"].as_str())
            .map(str::to_string)
            .context("Step-up 完成响应缺少 proof")?;

        let deleted = dispatch(
            &runtime,
            "account.user",
            "delete_account",
            json!({ "confirmation": "delete my account" }),
            &[
                ("authorization", authorization.as_str()),
                ("x-step-up-proof", proof.as_str()),
            ],
            42_101,
        )
        .await?;
        ensure!(deleted.code == 0, "删除账号必须成功: {}", deleted.message);

        // 存储面：凭据材料已清空、status=deleted、password_hash 换成惰性有效 PHC。
        let (status, after_hash, totp_secret, totp_recovery_digest): (
            String,
            String,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, password_hash, totp_secret, totp_recovery_digest FROM users WHERE id = ?",
        )
        .bind(user_id)
        .fetch_one(control.pool())
        .await?;
        ensure!(status == "deleted", "删除后 status 必须为 deleted，实际 {status}");
        ensure!(after_hash != before_hash, "删除后 password_hash 必须被替换");
        ensure!(
            after_hash.starts_with("$argon2id$"),
            "删除后 password_hash 必须是惰性有效 PHC，实际 {after_hash}"
        );
        ensure!(totp_secret.is_none(), "删除后 totp_secret 必须置空");
        ensure!(totp_recovery_digest.is_none(), "删除后 totp_recovery_digest 必须置空");

        // 会话行与登录事件行必须被删除（PII 不再保留）。
        let session_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_session WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(control.pool())
                .await?;
        ensure!(session_count == 0, "删除后 user_session 必须为空，实际 {session_count}");
        let login_event_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM login_event WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(control.pool())
                .await?;
        ensure!(login_event_count == 0, "删除后 login_event 必须为空，实际 {login_event_count}");
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
