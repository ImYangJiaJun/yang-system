//! 权限组接入 Token 签发的真实库集成测试。
//!
//! 回归对象（plan Task 5 Step 6）：`GroupGrantResolver` 必须把用户所在组的
//! 有效权限并入登录签发的 Access Token claims，且**只并入 permissions**——
//! 组名绝不进入 roles（spec §6.1）。单元测试只覆盖纯函数的合并语义，
//! 「组事实 → Token claims」这条跨层链路只有在真实 MySQL/Redis 上才能验证。

mod common;

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::collections::BTreeSet;
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
const GROUP_READ_PERMISSION: &str = "access.grants.read";
const SYSTEM_ADMIN_GROUP_KEY: &str = "system_admin";

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

/// 登录签发与组解析都要走完整凭据版本语义，故与 account 侧集成测试同参数。
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

/// 装配用的 TokenManager；校验侧的实例以同一套参数构造，因此能验证同一批 Token。
fn token_manager() -> TokenManager {
    TokenManager::new_symmetric_keyring(
        "permission-groups-active".to_string(),
        "permission-groups-secret-at-least-32-bytes",
        Vec::new(),
        Algorithm::HS256,
        "yang-system-permission-groups".to_string(),
        "yang-system-permission-groups-api".to_string(),
        3600,
        2_592_000,
    )
    .unwrap_or_else(|error| panic!("权限组测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "permission-groups-step-up-secret-32-bytes",
            "yang-system-permission-groups-step-up",
            "yang-system-permission-groups-sensitive",
        )
        .unwrap_or_else(|error| panic!("权限组测试 Step-up manager 应构建成功: {error}")),
    )
}

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接权限组测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取权限组测试数据库名失败")?;
    let name = name.context("权限组测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行权限组测试"
    );
    Ok(database)
}

async fn connect_test_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "权限组测试 Redis URL 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, redis_config())
        .await
        .context("连接权限组测试 Redis 失败")
}

/// 清空测试库：组事实表必须先于 `users` / `permission_group` 删除（外键 RESTRICT）。
async fn reset_database(database: &Database) -> anyhow::Result<()> {
    for table in [
        "user_group",
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
            .with_context(|| format!("清理权限组测试表失败: {table}"))?;
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

/// 注册并登录一个账号，返回（access token, user id）。
async fn register_and_login(
    app: &BuiltApp,
    control: &Database,
    suffix: u128,
    peer_port: u16,
) -> anyhow::Result<(String, i64)> {
    let username = format!("group_{suffix}");
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
    let token = login(app, &username, peer_port).await?;
    let user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(&username)
        .fetch_one(control.pool())
        .await
        .context("读取新注册用户 ID 失败")?;
    Ok((token, user_id))
}

async fn login(app: &BuiltApp, username: &str, peer_port: u16) -> anyhow::Result<String> {
    let response = dispatch(
        app,
        "account.user",
        "login",
        json!({ "username": username, "password": PASSWORD }),
        &[],
        peer_port,
    )
    .await?;
    ensure!(response.code == 0, "登录必须成功: {}", response.message);
    access_token(&response)
}

fn string_list(value: &Value, field: &str) -> anyhow::Result<Vec<String>> {
    value
        .as_array()
        .with_context(|| format!("claims 缺少 {field} 数组"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .with_context(|| format!("claims {field} 的元素必须是字符串"))
        })
        .collect()
}

/// 从 Access Token 中取（permissions, roles）。
fn token_grants(token: &str) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let claims = token_manager()
        .verify_token(token)
        .map_err(|error| anyhow::anyhow!("校验 Access Token 失败: {error}"))?;
    // 权限与角色都是 claims 顶层字段（AppClaims 经 serde(flatten) 展平）。
    Ok((
        string_list(&claims.custom["permissions"], "permissions")?,
        string_list(&claims.custom["roles"], "roles")?,
    ))
}

fn token_permissions(token: &str) -> anyhow::Result<Vec<String>> {
    Ok(token_grants(token)?.0)
}

/// 冻结 Catalog 声明的全部权限——内置全权组的期望值。
///
/// 在测试侧独立重算目录（而不是复用 `project_permissions`），使断言真的能
/// 抓住「组解析漏权限 / 多权限」的实现错误。
fn declared_permissions(app: &BuiltApp) -> Vec<String> {
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for addon in app.catalog().addons() {
        for module in &addon.modules {
            for permission in &module.default_permissions {
                declared.insert(permission.clone());
            }
            for action in module.actions() {
                for permission in &action.permissions {
                    declared.insert(permission.clone());
                }
            }
        }
    }
    declared.into_iter().collect()
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

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("权限组测试数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("权限组测试 Redis 清理失败")),
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
        "permission-groups-{}",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn group_membership_flows_into_the_issued_access_token() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let runtime = build_application(&control, &redis).await?;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (before_token, user_id) =
            register_and_login(&runtime, &control, suffix, 42_301).await?;
        let before = token_permissions(&before_token)?;
        ensure!(
            !before.iter().any(|p| p == GROUP_READ_PERMISSION),
            "入组前 Token 不应含组权限，实际 {before:?}"
        );

        // 组管理 Action 在 Task 10/11/12 才接入，这里按受信 writer 的列口径直写组事实
        // （组生命周期只经 `GroupRepository`，测试夹具是唯一例外，与既有集成测试同例）。
        let group_id = insert_group(&control, "reporter", user_id).await?;
        insert_item(&control, group_id, GROUP_READ_PERMISSION, user_id).await?;
        insert_membership(&control, user_id, group_id, user_id).await?;

        let after_token = login(&runtime, &format!("group_{suffix}"), 42_301).await?;
        let (after, roles) = token_grants(&after_token)?;
        ensure!(
            after.iter().any(|p| p == GROUP_READ_PERMISSION),
            "入组后 Token 必须含组内权限 {GROUP_READ_PERMISSION}，实际 {after:?}"
        );
        ensure!(
            after.len() == before.len() + 1,
            "组解析只应新增一条权限：入组前 {before:?}，入组后 {after:?}"
        );
        ensure!(roles == ["user"], "组名绝不进入角色集合，实际 {roles:?}");
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn system_admin_group_token_carries_the_whole_catalog() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        let runtime = build_application(&control, &redis).await?;
        let expected = declared_permissions(&runtime);
        ensure!(
            !expected.is_empty(),
            "冻结 Catalog 必须声明权限，否则断言无意义"
        );

        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let (_, user_id) = register_and_login(&runtime, &control, suffix, 42_302).await?;
        // 内置全权组的条目由目录计算，表里没有行也应签发整个目录。
        let group_id = insert_group(&control, SYSTEM_ADMIN_GROUP_KEY, 0).await?;
        insert_membership(&control, user_id, group_id, user_id).await?;

        let token = login(&runtime, &format!("group_{suffix}"), 42_302).await?;
        let (permissions, roles) = token_grants(&token)?;
        ensure!(
            permissions == expected,
            "内置全权组的 Token 必须恰好含全部目录权限：缺少 {:?}，多出 {:?}",
            expected
                .iter()
                .filter(|p| !permissions.contains(p))
                .collect::<Vec<_>>(),
            permissions
                .iter()
                .filter(|p| !expected.contains(p))
                .collect::<Vec<_>>()
        );
        ensure!(roles == ["user"], "组名绝不进入角色集合，实际 {roles:?}");
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
