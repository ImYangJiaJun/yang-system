//! 权限持有者下钻（access.grants.list_holders）的真实库集成测试。
//!
//! 回归对象：`list_holders` 的权限门禁（无 `access.grants.read` → 403）、
//! 未声明权限 fail-closed（400），以及直授/权限组两侧的投影形状——过期直授行
//! 必须带 `expired` 派生标记展示，组侧必须带成员数，空持有者返回空数组。

mod common;

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{
    ApiResponse, Request, RequestMeta, StepUpChallenge, StepUpManager, STEP_UP_PROOF_HEADER,
};
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
/// 被测权限：保留键（目录恒声明、可授予），且不是管理员等价权限（授予无需 Step-up）。
const HOLD_PERMISSION: &str = "access.groups.read";

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
        "grant-holders-active".to_string(),
        "grant-holders-secret-at-least-32-bytes",
        Vec::new(),
        Algorithm::HS256,
        "yang-system-grant-holders".to_string(),
        "yang-system-grant-holders-api".to_string(),
        3600,
        2_592_000,
    )
    .unwrap_or_else(|error| panic!("持有者测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "grant-holders-step-up-secret-32-bytes",
            "yang-system-grant-holders-step-up",
            "yang-system-grant-holders-sensitive",
        )
        .unwrap_or_else(|error| panic!("持有者测试 Step-up manager 应构建成功: {error}")),
    )
}

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接持有者测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取持有者测试数据库名失败")?;
    let name = name.context("持有者测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行持有者测试"
    );
    Ok(database)
}

async fn connect_test_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "持有者测试 Redis URL 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, redis_config())
        .await
        .context("连接持有者测试 Redis 失败")
}

/// 清空测试库：授权事实与组事实表先于 `users` 删除（外键 RESTRICT）。
async fn reset_database(database: &Database) -> anyhow::Result<()> {
    for table in [
        "user_group",
        "authz_grant",
        "permission_group_item",
        "permission_group",
        "system_owner",
        "user_avatar",
        "password_reset_token",
        "user_session",
        "login_event",
        "audit_event",
        "authorization_outbox",
        "users",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(database.pool())
            .await
            .with_context(|| format!("清理持有者测试表失败: {table}"))?;
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

/// 同 `permission_groups_integration` 的 dispatch，额外携带 query 参数：
/// `params!` 生成的 decode 对 `source = query` 的字段只读 `request.query`。
async fn dispatch_with_query(
    app: &BuiltApp,
    module: &str,
    action: &str,
    body: Value,
    headers: &[(&str, &str)],
    peer_port: u16,
    query: &[(&str, &str)],
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    for (name, value) in query {
        request = request.query(*name, *value);
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

/// 注册并登录一个账号，返回（access token, user id, username）。
async fn register_and_login(
    app: &BuiltApp,
    control: &Database,
    suffix: u128,
    peer_port: u16,
) -> anyhow::Result<(String, i64, String)> {
    let username = format!("holder_{suffix}");
    let email = format!("{username}@example.test");
    let registered = dispatch_with_query(
        app,
        "account.user",
        "request_registration_email",
        json!({ "email": email }),
        &[],
        peer_port,
        &[],
    )
    .await?;
    ensure!(
        registered.code == 0,
        "注册邮件请求必须成功: {}",
        registered.message
    );
    let code = take_registration_code(&email)?;
    let registered = dispatch_with_query(
        app,
        "account.user",
        "register",
        json!({ "username": username, "password": PASSWORD, "email": email, "email_code": code }),
        &[],
        peer_port,
        &[],
    )
    .await?;
    ensure!(registered.code == 0, "注册必须成功: {}", registered.message);
    let token = login(app, &username, peer_port).await?;
    let user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(&username)
        .fetch_one(control.pool())
        .await
        .context("读取新注册用户 ID 失败")?;
    Ok((token, user_id, username))
}

/// 用操作者本人的密码完成一次 Step-up 重认证，返回一次性 proof。
async fn complete_step_up(
    app: &BuiltApp,
    username: &str,
    challenge: StepUpChallenge,
    peer_port: u16,
) -> anyhow::Result<String> {
    let completed = dispatch_with_query(
        app,
        "account.user",
        "step_up_complete",
        json!({
            "challenge": challenge.challenge,
            "credentials": { "username": username, "password": PASSWORD },
        }),
        &[],
        peer_port,
        &[],
    )
    .await?;
    completed
        .data
        .as_ref()
        .and_then(|data| data["proof"].as_str())
        .map(str::to_string)
        .context("Step-up 完成响应缺少 proof")
}

/// 驱动受 Step-up 保护的授予：先无 proof 触发 challenge，用密码重认证换
/// 一次性 proof，再带 proof 重试同一次调用（grant_permission 是模块装配的
/// 写操作，恒受 Step-up 守卫保护）。
async fn grant_permission_with_step_up(
    app: &BuiltApp,
    operator_token: &str,
    operator_username: &str,
    user_id: i64,
    permission: &str,
    peer_port: u16,
) -> anyhow::Result<ApiResponse> {
    let authorization = format!("Bearer {operator_token}");
    let body = json!({ "user_id": user_id, "permission": permission });
    match dispatch_with_query(
        app,
        "access.grants",
        "grant_permission",
        body.clone(),
        &[("authorization", authorization.as_str())],
        peer_port,
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => {
            let proof = complete_step_up(app, operator_username, challenge, peer_port).await?;
            Ok(dispatch_with_query(
                app,
                "access.grants",
                "grant_permission",
                body,
                &[
                    ("authorization", authorization.as_str()),
                    (STEP_UP_PROOF_HEADER, proof.as_str()),
                ],
                peer_port,
                &[],
            )
            .await?)
        }
        Ok(response) => Ok(response),
        Err(other) => Err(anyhow::Error::new(other)),
    }
}

async fn login(app: &BuiltApp, username: &str, peer_port: u16) -> anyhow::Result<String> {
    let response = dispatch_with_query(
        app,
        "account.user",
        "login",
        json!({ "username": username, "password": PASSWORD }),
        &[],
        peer_port,
        &[],
    )
    .await?;
    ensure!(response.code == 0, "登录必须成功: {}", response.message);
    access_token(&response)
}

/// `occurred_at` 由 ORM 在插入时填充（表声明里没有数据库默认值），
/// 因此直写夹具必须显式给出时间戳列，否则严格模式报 1364。
async fn insert_group(control: &Database, group_key: &str, created_by: i64) -> anyhow::Result<i64> {
    sqlx::query(
        "INSERT INTO permission_group (group_key, title, created_by, occurred_at) \
         VALUES (?, ?, ?, UNIX_TIMESTAMP())",
    )
    .bind(group_key)
    .bind(format!("集成测试组 {group_key}"))
    .bind(created_by)
    .execute(control.pool())
    .await
    .context("插入权限组失败")?;
    sqlx::query_scalar("SELECT id FROM permission_group WHERE group_key = ?")
        .bind(group_key)
        .fetch_one(control.pool())
        .await
        .context("回查权限组 ID 失败")
}

async fn insert_item(
    control: &Database,
    group_id: i64,
    permission: &str,
    granted_by: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO permission_group_item (group_id, permission, granted_by, occurred_at) \
         VALUES (?, ?, ?, UNIX_TIMESTAMP())",
    )
    .bind(group_id)
    .bind(permission)
    .bind(granted_by)
    .execute(control.pool())
    .await
    .context("插入组条目失败")?;
    Ok(())
}

async fn insert_membership(
    control: &Database,
    user_id: i64,
    group_id: i64,
    granted_by: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO user_group (user_id, group_id, granted_by, occurred_at) \
         VALUES (?, ?, ?, UNIX_TIMESTAMP())",
    )
    .bind(user_id)
    .bind(group_id)
    .bind(granted_by)
    .execute(control.pool())
    .await
    .context("插入组成员失败")?;
    Ok(())
}

/// 直写一条已过期的直授行（过期时间在过去）：过期行按审计要求保留在表里，
/// 下钻视图必须把它们带 `expired` 派生标记展示出来。
async fn insert_expired_grant(
    control: &Database,
    user_id: i64,
    permission: &str,
    granted_by: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO authz_grant (user_id, permission, granted_by, occurred_at, expires_at) \
         VALUES (?, ?, ?, UNIX_TIMESTAMP(), UNIX_TIMESTAMP() - 100)",
    )
    .bind(user_id)
    .bind(permission)
    .bind(granted_by)
    .execute(control.pool())
    .await
    .context("插入过期直授行失败")?;
    Ok(())
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("持有者测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("持有者测试 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

async fn build_application(control: &Database, redis: &RedisClient) -> anyhow::Result<BuiltApp> {
    sync_with_database(
        connect_test_database().await?,
        database_config(),
        security_settings(),
    )
    .await?;
    let deployment = format!(
        "grant-holders-{}",
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
    Ok(build_app(tools, security_settings())?.runtime)
}

/// 框架级缺权限判定：无 `access.grants.read` 的账号下钻权限持有者必须 403。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn holders_require_the_grants_read_permission() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let app = build_application(&control, &redis).await?;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        // 引导 claimer 会把首个注册账号变成系统管理员，先消费掉那个名额。
        register_and_login(&app, &control, suffix - 1, 42_500).await?;
        let (token, _, _) = register_and_login(&app, &control, suffix, 42_501).await?;

        let authorization = format!("Bearer {token}");
        match dispatch_with_query(
            &app,
            "access.grants",
            "list_holders",
            json!({}),
            &[("authorization", authorization.as_str())],
            42_501,
            &[("permission", HOLD_PERMISSION)],
        )
        .await
        {
            Err(BaseError::PermissionDenied(message)) => ensure!(
                message.contains("access.grants.read"),
                "拒绝信息必须点名缺失的权限，实际 {message}"
            ),
            Ok(response) => panic!(
                "缺权限调用必须被拒，实际成功（code={}，{}）",
                response.code, response.message
            ),
            Err(other) => panic!("缺权限必须 PermissionDenied→403，实际 {other}"),
        }
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

/// 未声明的权限不允许下钻：与授予闸门同一判据，fail-closed 返回 400。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn undeclared_permission_is_rejected_with_400() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let app = build_application(&control, &redis).await?;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (token, _, _) = register_and_login(&app, &control, suffix, 42_502).await?;

        let authorization = format!("Bearer {token}");
        match dispatch_with_query(
            &app,
            "access.grants",
            "list_holders",
            json!({}),
            &[("authorization", authorization.as_str())],
            42_502,
            &[("permission", "not.declared.anywhere")],
        )
        .await
        {
            Err(BaseError::ParamInvalid(field, _)) => ensure!(
                field == "permission",
                "未声明权限必须按 permission 参数错误拒绝，实际字段 {field}"
            ),
            Ok(response) => panic!(
                "未声明权限必须被拒，实际成功（code={}，{}）",
                response.code, response.message
            ),
            Err(other) => panic!("未声明权限必须 ParamInvalid→400，实际 {other}"),
        }
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

/// 数据正确性：直授侧（含过期行 + expired 派生标记）与组侧（组事实 + 成员数）
/// 合并成一份响应；空持有者返回空数组。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn holders_combine_direct_rows_and_groups() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let app = build_application(&control, &redis).await?;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        // 首个注册账号被引导进全权组：用作操作者。
        let (admin_token, admin_id, admin_username) =
            register_and_login(&app, &control, suffix - 1, 42_503).await?;
        // 第二/第三个账号是零权限普通用户（夹具直写它们的持有事实）。
        let (_, direct_user_id, _) = register_and_login(&app, &control, suffix, 42_504).await?;
        let (_, expired_user_id, _) =
            register_and_login(&app, &control, suffix + 1, 42_505).await?;

        // 直授侧活跃行走真实授予链路（幂等 + 授权版本 + 审计）；授予是模块装配的
        // 写操作，恒受 Step-up 守卫保护，夹具要带 proof 重试。
        let granted = grant_permission_with_step_up(
            &app,
            &admin_token,
            &admin_username,
            direct_user_id,
            HOLD_PERMISSION,
            42_503,
        )
        .await?;
        ensure!(granted.code == 0, "授予必须成功: {}", granted.message);

        // 直授侧过期行：审计口径要求行保留，下钻视图必须展示且标记 expired。
        insert_expired_grant(&control, expired_user_id, HOLD_PERMISSION, admin_id).await?;

        // 组侧：组持有条目 + 一名成员。
        let group_id = insert_group(&control, "ops", admin_id).await?;
        insert_item(&control, group_id, HOLD_PERMISSION, admin_id).await?;
        insert_membership(&control, direct_user_id, group_id, admin_id).await?;

        let response = dispatch_with_query(
            &app,
            "access.grants",
            "list_holders",
            json!({}),
            &[("authorization", format!("Bearer {admin_token}").as_str())],
            42_503,
            &[("permission", HOLD_PERMISSION)],
        )
        .await?;
        ensure!(
            response.code == 0,
            "持权限下钻必须成功: {}",
            response.message
        );
        let data = response
            .data
            .as_ref()
            .unwrap_or_else(|| panic!("下钻响应缺少 data: {response:?}"));

        // 直授侧：活跃行（expired=false、永久）与过期行（expired=true）都在。
        let direct = data["direct"]
            .as_array()
            .with_context(|| format!("direct 必须是数组，实际 {data}"))?;
        assert_eq!(direct.len(), 2, "两条直授行都要展示，实际 {direct:?}");
        let by_user = |user_id: i64| -> anyhow::Result<&Value> {
            direct
                .iter()
                .find(|entry| entry["user_id"].as_i64() == Some(user_id))
                .with_context(|| format!("direct 缺少用户 {user_id} 的行: {direct:?}"))
        };
        let active = by_user(direct_user_id)?;
        assert_eq!(active["granted_by"].as_i64(), Some(admin_id));
        assert_eq!(
            active["expires_at"],
            Value::Null,
            "永久直授 expires_at 为 null"
        );
        assert_eq!(active["expired"].as_bool(), Some(false));
        assert!(
            active["occurred_at"].as_i64().is_some(),
            "occurred_at 必须带出: {active}"
        );
        let expired = by_user(expired_user_id)?;
        assert_eq!(expired["expired"].as_bool(), Some(true), "过期行必须带标记");
        assert!(
            expired["expires_at"].as_i64().is_some(),
            "过期行必须带过期时间: {expired}"
        );

        // 组侧：组事实 + 成员数。
        let groups = data["groups"]
            .as_array()
            .with_context(|| format!("groups 必须是数组，实际 {data}"))?;
        assert_eq!(groups.len(), 1, "只有一个组持有该权限，实际 {groups:?}");
        let group = &groups[0];
        assert_eq!(group["group_key"].as_str(), Some("ops"));
        assert_eq!(group["title"].as_str(), Some("集成测试组 ops"));
        assert_eq!(group["member_count"].as_i64(), Some(1));

        // 空持有者：声明的权限无人持有时返回空数组，不是错误。
        let empty = dispatch_with_query(
            &app,
            "access.grants",
            "list_holders",
            json!({}),
            &[("authorization", format!("Bearer {admin_token}").as_str())],
            42_503,
            &[("permission", "access.groups.write")],
        )
        .await?;
        ensure!(empty.code == 0, "空持有者必须成功返回: {}", empty.message);
        let data = empty
            .data
            .as_ref()
            .unwrap_or_else(|| panic!("空持有者响应缺少 data: {empty:?}"));
        assert_eq!(data["direct"], json!([]), "空持有者 direct 必须是空数组");
        assert_eq!(data["groups"], json!([]), "空持有者 groups 必须是空数组");
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
