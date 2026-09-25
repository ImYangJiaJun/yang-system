//! 逐台撤销的对抗性集成测试：撤销一台设备后，其 refresh token 必须被拒绝。
//!
//! 回归对象（路线图 C-1d + 评审 2026-09-14 致命缺陷）：`revoke_session` 曾只把
//! access token 的 jti 写入黑名单，而 refresh 轮换校验的是 refresh token 自身的 jti
//! （二者独立生成），导致被踢设备仍可凭 refresh cookie 无限续期。本测试在
//! `issue_refresh_credential_version = false` 下运行，使 revoke_session 不挂 Step-up，
//! 从而聚焦「撤销 → refresh 失败」的核心语义。

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

/// revoke_session 现始终受 Step-up 保护（与凭据变更开关无关），测试须走
/// challenge→proof 流程；开启凭据版本签发使 delete_account 等也进入 step_up_targets。
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
        "session-revoke-active".to_string(),
        "session-revoke-secret-at-least-32-bytes",
        Vec::new(),
        Algorithm::HS256,
        "yang-system-session-revoke".to_string(),
        "yang-system-session-revoke-api".to_string(),
        3600,
        2_592_000,
    )
    .unwrap_or_else(|error| panic!("会话撤销 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "session-revoke-step-up-secret-32-bytes",
            "yang-system-session-revoke-step-up",
            "yang-system-session-revoke-sensitive",
        )
        .unwrap_or_else(|error| panic!("会话撤销 Step-up manager 应构建成功: {error}")),
    )
}

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接会话撤销 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取会话撤销数据库名失败")?;
    let name = name.context("会话撤销连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行会话撤销集成测试"
    );
    Ok(database)
}

async fn connect_test_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "会话撤销 Redis URL 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, redis_config())
        .await
        .context("连接会话撤销 Redis 失败")
}

async fn reset_database(database: &Database) -> anyhow::Result<()> {
    // 先删引用 users 的子表，再删 users，避免外键约束阻断。
    for table in [
        "user_avatar",
        "password_reset_token",
        "user_session",
        "login_event",
        "audit_event",
        "authorization_outbox",
        // 组与授权事实表：首账号引导会把注册者写进内置全权组，`user_group` 与
        // `system_owner` 因此对 `users` 持有 RESTRICT 外键。漏删它们时收尾
        // `DROP users` 会以 3730 失败（`account_deletion_integration` 的夹具同因）。
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
            .with_context(|| format!("清理会话撤销测试表失败: {table}"))?;
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

/// 直接派发并返回原始结果（不强制 code==0，供「撤销后 refresh 应失败」断言）。
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

fn refresh_cookie(response: &ApiResponse) -> anyhow::Result<String> {
    response
        .response_headers()
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .find_map(|(_, value)| {
            value
                .split(';')
                .next()
                .and_then(|cookie| cookie.trim().strip_prefix("yang_refresh="))
                .filter(|token| !token.is_empty())
                .map(str::to_owned)
        })
        .context("登录响应缺少轮换后的 yang_refresh Cookie")
}

async fn register_and_login(
    app: &BuiltApp,
    suffix: u128,
    peer_port: u16,
) -> anyhow::Result<(String, String)> {
    let username = format!("revoke_{suffix}");
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
    Ok((access_token(&login)?, refresh_cookie(&login)?))
}

/// 从会话列表取当前设备的 session_id（列表须恰有一台活跃设备）。
async fn current_session_id(app: &BuiltApp, token: &str, peer_port: u16) -> anyhow::Result<String> {
    let authorization = format!("Bearer {token}");
    let response = dispatch(
        app,
        "account.user",
        "list_sessions",
        json!({}),
        &[("authorization", authorization.as_str())],
        peer_port,
    )
    .await?;
    ensure!(
        response.code == 0,
        "list_sessions 必须成功: {}",
        response.message
    );
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data["sessions"].as_array())
        .context("list_sessions 响应缺少 sessions 数组")?;
    ensure!(
        sessions.len() == 1,
        "预期恰有一台活跃设备，实际 {}",
        sessions.len()
    );
    sessions[0]["session_id"]
        .as_str()
        .map(str::to_string)
        .context("会话行缺少 session_id")
}

/// 统计指定 Action 的审计事件行数（验证成功审计与业务写同事务落库）。
async fn audit_event_count(database: &Database, action: &str) -> anyhow::Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_event WHERE action = ?")
        .bind(action)
        .fetch_one(database.pool())
        .await?;
    Ok(count)
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("会话撤销数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("会话撤销 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn revoking_a_session_rejects_its_refresh_token() -> anyhow::Result<()> {
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
            "session-revoke-{}",
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

        // 1. 登录，得到 access token + refresh cookie + 一个会话行。
        let (token, cookie) = register_and_login(&runtime, suffix, 42_001).await?;

        // 2. 轮换一次：验证 session_id 跨轮换继承，并让 user_session 记录轮换后的
        //    refresh jti（更强的断言——撤销命中的是「已轮换」的 refresh token）。
        let rotated = dispatch(
            &runtime,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", format!("yang_refresh={cookie}").as_str())],
            42_001,
        )
        .await?;
        ensure!(
            rotated.code == 0,
            "撤销前 refresh 必须成功: {}",
            rotated.message
        );
        let rotated_cookie = refresh_cookie(&rotated)?;

        // 3. 取会话 ID（列表应仍只显示一台设备，说明 session_id 在轮换后未丢失）。
        let session_id = current_session_id(&runtime, &token, 42_001).await?;

        // 4. 撤销该会话（revoke_session 始终受 Step-up 保护，先触发 challenge 再重认证）。
        let username = format!("revoke_{suffix}");
        let authorization = format!("Bearer {token}");
        let challenge = match dispatch(
            &runtime,
            "account.user",
            "revoke_session",
            json!({ "session_id": session_id }),
            &[("authorization", authorization.as_str())],
            42_001,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
            other => {
                anyhow::bail!("撤销会话缺少 proof 必须返回 Step-up challenge，实际: {other:?}")
            }
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
            42_001,
        )
        .await?;
        let proof = completed
            .data
            .as_ref()
            .and_then(|data| data["proof"].as_str())
            .map(str::to_string)
            .context("Step-up 完成响应缺少 proof")?;
        let revoked = dispatch(
            &runtime,
            "account.user",
            "revoke_session",
            json!({ "session_id": session_id }),
            &[
                ("authorization", authorization.as_str()),
                ("x-step-up-proof", proof.as_str()),
            ],
            42_001,
        )
        .await?;
        ensure!(revoked.code == 0, "撤销会话必须成功: {}", revoked.message);

        // 4a. 成功撤销必须写入一条审计事件（审计与业务行标记同事务原子提交）。
        let audit_count = audit_event_count(&control, "account.user.revoke_session").await?;
        ensure!(
            audit_count == 1,
            "撤销会话应写入一条成功审计事件，实际 {audit_count}"
        );

        // 5. 被撤销设备的 refresh token 必须被拒绝（核心回归：此前只拉黑 access jti，
        //    refresh jti 未被拉黑，此处会错误地成功）。TokenRevoked 经 dispatch_context
        // 以 Err 返回，业务错误则以 Ok(response.code != 0) 返回，两种都算「已拒绝」。
        let refresh_after_revoke = dispatch(
            &runtime,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", format!("yang_refresh={rotated_cookie}").as_str())],
            42_001,
        )
        .await;
        let still_valid = match refresh_after_revoke {
            Ok(response) => response.code == 0,
            Err(_) => false,
        };
        ensure!(
            !still_valid,
            "被撤销设备的 refresh token 必须被拒绝，但 refresh 仍然成功"
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
