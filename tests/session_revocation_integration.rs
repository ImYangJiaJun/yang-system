//! 逐台撤销的对抗性集成测试：撤销一台设备后，其 refresh token 必须被拒绝。
//!
//! 回归对象（路线图 C-1d + 评审 2026-09-14 致命缺陷）：`revoke_session` 曾只把
//! access token 的 jti 写入黑名单，而 refresh 轮换校验的是 refresh token 自身的 jti
//! （二者独立生成），导致被踢设备仍可凭 refresh cookie 无限续期。本测试在
//! 开启凭据版本签发；同时验证 Access、Refresh 与 Redis 丢失下的持久化撤销。

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::SecuritySettings;
use yang_system::schema::sync_with_database;

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

/// 开启凭据版本签发。
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
        // 组与授权事实表：首账号引导会把注册者写进内置全权组，`user_group`
        // 因此对 `users` 持有 RESTRICT 外键（`fk_user_group_user`）；漏删它时
        // 收尾 `DROP users` 会以 3730 失败。`system_owner` 表没有外键，但它存的是
        // 「引导已发生过」的哨兵行，不删会让下一轮首账号不再晋升管理员；
        // `authz_grant` 同样无外键，但按 `user_id` 计数会串到下一轮。
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
    let registered = dispatch(
        app,
        "account.user",
        "register",
        json!({ "username": username, "password": PASSWORD }),
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

async fn session_test_app(database: &Database, redis: &RedisClient) -> anyhow::Result<BuiltApp> {
    reset_database(database).await?;
    reset_redis(redis).await?;
    sync_with_database(
        connect_test_database().await?,
        database_config(),
        security_settings(),
    )
    .await?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(Database::from_pool(
                database.pool().clone(),
                database_config(),
            )?)
            .cache(redis.clone())
            .extension(AuthorizationVersionCache::new(
                redis.clone(),
                format!(
                    "durable-session-{}",
                    SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
                ),
            )?)
            .token(token_manager())
            .build()?,
    );
    Ok(build_app(tools, security_settings())?.runtime)
}

async fn refresh(app: &BuiltApp, cookie: &str) -> Result<ApiResponse, BaseError> {
    dispatch(
        app,
        "account.user",
        "refresh",
        json!({}),
        &[("cookie", format!("yang_refresh={cookie}").as_str())],
        42_003,
    )
    .await
}

async fn me(app: &BuiltApp, token: &str) -> Result<ApiResponse, BaseError> {
    dispatch(
        app,
        "account.user",
        "me",
        json!({}),
        &[("authorization", format!("Bearer {token}").as_str())],
        42_003,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn durable_session_revocation_rejects_historical_access_after_redis_loss(
) -> anyhow::Result<()> {
    let database = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    let app = session_test_app(&database, &redis).await?;
    let outcome = async {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (old_access, old_cookie) = register_and_login(&app, suffix, 42_003).await?;
        let session_id = current_session_id(&app, &old_access, 42_003).await?;
        let rotated = refresh(&app, &old_cookie).await?;
        ensure!(rotated.code == 0, "正常轮换必须成功");
        let new_access = access_token(&rotated)?;
        let other_device = dispatch(
            &app,
            "account.user",
            "login",
            json!({"username": format!("revoke_{suffix}"), "password": PASSWORD}),
            &[],
            42_004,
        )
        .await?;
        ensure!(other_device.code == 0, "同账号另一设备必须能登录");
        let other_access = access_token(&other_device)?;
        let revoked = dispatch(
            &app,
            "account.user",
            "revoke_session",
            json!({"session_id":session_id}),
            &[("authorization", format!("Bearer {other_access}").as_str())],
            42_004,
        )
        .await?;
        ensure!(revoked.code == 0, "撤销必须成功");
        reset_redis(&redis).await?;
        for access in [&old_access, &new_access] {
            let result = me(&app, access).await;
            ensure!(
                matches!(result, Err(BaseError::TokenRevoked)),
                "已撤销 session 的所有历史 Access 必须在 Redis 丢失后仍被拒绝"
            );
        }
        ensure!(
            me(&app, &other_access).await?.code == 0,
            "逐台撤销不得影响另一设备"
        );
        ensure!(
            refresh(&app, &refresh_cookie(&other_device)?).await?.code == 0,
            "另一设备的 Refresh 必须仍可用"
        );
        Ok(())
    }
    .await;
    finish_with_cleanup(
        outcome,
        reset_database(&database).await,
        reset_redis(&redis).await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn durable_refresh_consumption_survives_empty_redis() -> anyhow::Result<()> {
    let database = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    let app = session_test_app(&database, &redis).await?;
    let outcome = async {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (_, first) = register_and_login(&app, suffix, 42_005).await?;
        let rotated = refresh(&app, &first).await?;
        ensure!(rotated.code == 0, "首次轮换必须成功");
        let second = refresh_cookie(&rotated)?;
        reset_redis(&redis).await?;
        let replay = refresh(&app, &first).await;
        ensure!(
            matches!(replay, Err(BaseError::TokenRevoked)),
            "Redis 变空后已消费的 Refresh 仍必须被拒绝"
        );
        ensure!(
            refresh(&app, &second).await?.code == 0,
            "拒绝旧 JTI 不得覆盖当前 Refresh 或撤销其他设备"
        );
        Ok(())
    }
    .await;
    finish_with_cleanup(
        outcome,
        reset_database(&database).await,
        reset_redis(&redis).await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn durable_session_rejects_legacy_and_wrong_owner_claims() -> anyhow::Result<()> {
    let database = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    let app = session_test_app(&database, &redis).await?;
    let outcome = async {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (access, cookie) = register_and_login(&app, suffix, 42_006).await?;
        let (_, stranger_cookie) = register_and_login(&app, suffix + 1, 42_007).await?;
        let manager = token_manager();
        let access_claims = manager.verify_token(&access)?;
        let refresh_claims = manager.verify_token(&cookie)?;
        let stranger = manager.verify_token(&stranger_cookie)?;
        for session in [
            Value::Null,
            Value::String("missing-session".into()),
            stranger.custom["session_id"].clone(),
        ] {
            let mut custom = access_claims.custom.clone();
            custom["session_id"] = session.clone();
            let forged_access = manager.generate_access_token(&access_claims.sub, custom)?;
            ensure!(
                matches!(me(&app, &forged_access).await, Err(BaseError::TokenRevoked)),
                "缺少会话或会话属于另一用户时不能使用 Access"
            );
            let mut custom = refresh_claims.custom.clone();
            custom["session_id"] = session;
            let forged_refresh =
                manager.generate_refresh_token_with_claims(&refresh_claims.sub, custom)?;
            ensure!(
                matches!(
                    refresh(&app, &forged_refresh).await,
                    Err(BaseError::TokenRevoked)
                ),
                "缺少会话或会话属于另一用户时不能使用 Refresh"
            );
        }
        ensure!(me(&app, &access).await?.code == 0, "有效会话仍必须可用");
        Ok(())
    }
    .await;
    finish_with_cleanup(
        outcome,
        reset_database(&database).await,
        reset_redis(&redis).await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn durable_session_storage_failure_is_unavailable_not_revoked() -> anyhow::Result<()> {
    let database = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    let app = session_test_app(&database, &redis).await?;
    let outcome = async {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (access, _) = register_and_login(&app, suffix, 42_009).await?;
        ensure!(
            me(&app, &access).await?.code == 0,
            "缓存命中前必须是合法会话"
        );
        let mut held = Vec::new();
        for _ in 0..20 {
            held.push(database.pool().acquire().await?);
        }
        let blocked = tokio::time::timeout(std::time::Duration::from_secs(6), me(&app, &access))
            .await
            .context("会话回源超过5秒预算仍未结束")?;
        ensure!(
            matches!(blocked, Err(BaseError::AuthorizationCheckUnavailable)),
            "会话连接池耗尽应返回依赖不可用"
        );
        drop(held);
        ensure!(
            me(&app, &access).await?.code == 0,
            "依赖恢复后原会话应继续有效"
        );
        sqlx::query("DROP TABLE user_session")
            .execute(database.pool())
            .await?;
        ensure!(
            matches!(
                me(&app, &access).await,
                Err(BaseError::AuthorizationCheckUnavailable)
            ),
            "数据库查询失败不能伪装成会话已撤销或继续用授权缓存放行"
        );
        Ok(())
    }
    .await;
    finish_with_cleanup(
        outcome,
        reset_database(&database).await,
        reset_redis(&redis).await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn durable_refresh_cannot_commit_after_concurrent_revocation() -> anyhow::Result<()> {
    let database = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    let app = session_test_app(&database, &redis).await?;
    let outcome = async {
        for change in [
            "UPDATE user_session SET revoked_at = UNIX_TIMESTAMP() WHERE session_id = ?",
            "UPDATE user_session SET refresh_jti = 'already-rotated' WHERE session_id = ?",
            "DELETE FROM user_session WHERE session_id = ?",
        ] {
            let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let (access, cookie) = register_and_login(&app, suffix, 42_008).await?;
            let session = current_session_id(&app, &access, 42_008).await?;
            let mut transaction = database.pool().begin().await?;
            sqlx::query("SELECT session_id FROM user_session WHERE session_id = ? FOR UPDATE")
                .bind(&session).fetch_one(&mut *transaction).await?;
            let change_after_snapshot = async {
                // 等实际 UPDATE 进入行锁等待，确保不是仅验证较早的快照检查。
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        let waiting: i64 = sqlx::query_scalar(
                            "SELECT COUNT(*) FROM performance_schema.data_lock_waits w JOIN performance_schema.data_locks l ON l.ENGINE=w.ENGINE AND l.ENGINE_LOCK_ID=w.REQUESTING_ENGINE_LOCK_ID WHERE l.OBJECT_SCHEMA=DATABASE() AND l.OBJECT_NAME='user_session'"
                        ).fetch_one(database.pool()).await?;
                        if waiting > 0 { break; }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Ok::<_, anyhow::Error>(())
                }).await.context("Refresh 未进入会话 UPDATE 行锁屏障")??;
                sqlx::query(change).bind(&session).execute(&mut *transaction).await?;
                transaction.commit().await?;
                Ok::<_, anyhow::Error>(())
            };
            let (rotated, changed) = tokio::join!(refresh(&app, &cookie), change_after_snapshot);
            changed?;
            ensure!(matches!(rotated, Err(BaseError::TokenRevoked)),
                "快照校验后撤销、推进代次或删除会话均必须阻止成功响应: {change}");
        }
        Ok(())
    }
    .await;
    finish_with_cleanup(
        outcome,
        reset_database(&database).await,
        reset_redis(&redis).await,
    )
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
                .extension(AuthorizationVersionCache::new(redis.clone(), deployment)?)
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

        // 4. 直接撤销该会话，无需 Step-up。
        let authorization = format!("Bearer {token}");
        let revoked = dispatch(
            &runtime,
            "account.user",
            "revoke_session",
            json!({ "session_id": session_id }),
            &[("authorization", authorization.as_str())],
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

/// 即使 jti 黑名单尚未写入，只要会话行被标记撤销，refresh 就必须 fail-closed。
///
/// 回归：refresh 路径曾完全不读 `user_session.revoked_at`，只依赖 `revoke_session`
/// 提交后的 best-effort jti 拉黑；并发轮换或拉黑失败会让被撤销设备继续续期。
/// 本用例直接改库标记 revoked_at、不写任何 jti 黑名单，隔离出会话行判定本身。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn refresh_rejects_a_session_row_marked_revoked_without_jti_blacklist() -> anyhow::Result<()>
{
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
            "session-row-revoke-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(Database::from_pool(
                    control.pool().clone(),
                    database_config(),
                )?)
                .cache(redis.clone())
                .extension(AuthorizationVersionCache::new(redis.clone(), deployment)?)
                .token(token_manager())
                .build()?,
        );
        let app = build_app(tools, security_settings())?;
        let runtime = app.runtime;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();

        let (token, cookie) = register_and_login(&runtime, suffix, 42_002).await?;
        let session_id = current_session_id(&runtime, &token, 42_002).await?;

        // 只标记会话行，不写任何 jti 黑名单：模拟 revoke_session 提交后拉黑失败，
        // 或攻击者在拉黑前并发轮换出新 jti 的窗口。
        sqlx::query("UPDATE user_session SET revoked_at = UNIX_TIMESTAMP() WHERE session_id = ?")
            .bind(&session_id)
            .execute(control.pool())
            .await?;

        let refresh_after_revoke = dispatch(
            &runtime,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", format!("yang_refresh={cookie}").as_str())],
            42_002,
        )
        .await;
        let still_valid = match refresh_after_revoke {
            Ok(response) => response.code == 0,
            Err(_) => false,
        };
        ensure!(
            !still_valid,
            "会话行 revoked_at 非空时 refresh 必须 fail-closed，即使 jti 未拉黑"
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
