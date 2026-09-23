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
        // Task 9 的引导 claimer 会把**首个**注册账号变成系统管理员（令牌因此带全目录
        // 权限），所以先消费掉那个名额——被测账号必须是零权限的普通用户。
        register_and_login(&runtime, &control, suffix - 1, 42_300).await?;
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
        // 首个注册账号被引导 claimer 建进内置全权组；这里要单独验证「成员关系 →
        // Token 全量」这条链，因此被测账号取第二个（夹具直写它的成员关系）。
        register_and_login(&runtime, &control, suffix - 1, 42_301).await?;
        let (_, user_id) = register_and_login(&runtime, &control, suffix, 42_302).await?;
        // 内置全权组由引导幂等创建，这里只查它的 id（不再直写，否则撞唯一键）。
        // 它的条目由目录计算，表里没有行也应签发整个目录。
        let group_id: i64 =
            sqlx::query_scalar("SELECT id FROM permission_group WHERE group_key = ?")
                .bind(SYSTEM_ADMIN_GROUP_KEY)
                .fetch_one(control.pool())
                .await
                .context("读取内置全权组 ID 失败")?;
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

// ---------------------------------------------------------------------------
// Task 10 起的组管理夹具与用例。
// ---------------------------------------------------------------------------

/// 组管理接口的测试夹具。
///
/// 把「注册账号 → 登录取令牌 → 受 Step-up 保护的组写操作 → 直接观测数据库事实」
/// 收敛在这里，用例只描述语义；后续任务（成员、条目、生命周期）在同一套夹具上追加。
///
/// 两处取舍写在明处：
/// 1. **成员写入直捣 `user_group`**：成员 Action 尚未落地，夹具只能按受信 writer
///    的列口径直写（与 Task 5 的集成测试同例），成员 Action 落地后应换成真实 Action。
/// 2. **HTTP 状态码由测试侧按同一套分类重算**：框架的映射是传输层私有实现，
///    集成测试拿不到，只能覆盖本文件会遇到的类别（见 `http_status`）。
mod harness {
    use super::common::take_registration_code;
    use super::{
        build_application, connect_test_database, connect_test_redis, dispatch, login,
        reset_database, reset_redis, PASSWORD, SYSTEM_ADMIN_GROUP_KEY,
    };
    use anyhow::{ensure, Context};
    use serde_json::{json, Value};
    use yang_base::action::{ApiResponse, STEP_UP_PROOF_HEADER};
    use yang_base::definition::BuiltApp;
    use yang_base::error::ErrorCategory;
    use yang_base::BaseError;

    /// 夹具固定的对端端口：注册与重认证限流按 IP 计数，阈值已放大到用例不会触发。
    const PEER_PORT: u16 = 52_400;

    /// 管理员身份：令牌，以及重认证所需的用户名与用户 ID。
    #[derive(Clone)]
    pub struct Admin {
        pub username: String,
        pub user_id: i64,
        pub token: String,
    }

    /// 装配一个「业务表已清空」的测试应用。
    ///
    /// 与 Task 9 的夹具同构，差别是装配前 DROP 掉业务表：组管理用例要求每个用例都
    /// 从零账号、零组事实开始，否则 `group_key` 唯一键会撞上一个用例的残留行。
    pub async fn build_test_app() -> BuiltApp {
        let control = connect_test_database()
            .await
            .unwrap_or_else(|error| panic!("连接测试库失败: {error}"));
        let redis = connect_test_redis()
            .await
            .unwrap_or_else(|error| panic!("连接测试 Redis 失败: {error}"));
        reset_database(&control)
            .await
            .unwrap_or_else(|error| panic!("清理测试库失败: {error}"));
        reset_redis(&redis)
            .await
            .unwrap_or_else(|error| panic!("清理测试 Redis 失败: {error}"));
        build_application(&control, &redis)
            .await
            .unwrap_or_else(|error| panic!("装配测试应用失败: {error}"))
    }

    /// 走真实注册路径创建账号（邮箱验证码 → 注册），返回新账号的用户 ID。
    pub async fn register_with_code(
        app: &BuiltApp,
        username: &str,
        email: &str,
    ) -> anyhow::Result<i64> {
        dispatch(
            app,
            "account.user",
            "request_registration_email",
            json!({ "email": email }),
            &[],
            PEER_PORT,
        )
        .await?;
        let code = take_registration_code(email)?;
        let registered = dispatch(
            app,
            "account.user",
            "register",
            json!({
                "username": username,
                "password": PASSWORD,
                "email": email,
                "email_code": code,
            }),
            &[],
            PEER_PORT,
        )
        .await?;
        ensure!(registered.code == 0, "注册必须成功: {}", registered.message);
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(username)
            .fetch_one(pool_of(app))
            .await
            .context("回查新注册账号 ID 失败")?;
        Ok(user_id)
    }

    /// 让首账号经真实注册路径成为系统管理员，返回其管理员身份。
    pub async fn bootstrap_admin(app: &BuiltApp) -> Admin {
        let username = "admin";
        let user_id = register_with_code(app, username, "admin@example.com")
            .await
            .unwrap_or_else(|error| panic!("引导管理员注册失败: {error}"));
        let token = login(app, username, PEER_PORT)
            .await
            .unwrap_or_else(|error| panic!("引导管理员登录失败: {error}"));
        Admin {
            username: username.to_string(),
            user_id,
            token,
        }
    }

    /// 建组（受 Step-up 保护），返回新组 ID。
    pub async fn create_group(app: &BuiltApp, admin: &Admin, group_key: &str, title: &str) -> i64 {
        let response = step_up_dispatch(
            app,
            "access.groups",
            "create_group",
            json!({ "group_key": group_key, "title": title }),
            admin,
        )
        .await
        .unwrap_or_else(|error| panic!("创建权限组 {group_key} 失败: {error}"));
        response
            .data
            .as_ref()
            .and_then(|data| data["id"].as_i64())
            .unwrap_or_else(|| panic!("创建权限组响应缺少 id: {}", response.message))
    }

    /// 删组并折算为 HTTP 状态码（成功为 200，其余由用例断言）。
    pub async fn delete_group_status(app: &BuiltApp, admin: &Admin, group_id: i64) -> u16 {
        match step_up_dispatch(
            app,
            "access.groups",
            "delete_group",
            json!({ "group_id": group_id }),
            admin,
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 组是否仍按 `group_key` 存在。
    pub async fn group_exists(app: &BuiltApp, group_key: &str) -> bool {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM permission_group WHERE group_key = ?")
                .bind(group_key)
                .fetch_one(pool_of(app))
                .await
                .unwrap_or_else(|error| panic!("统计权限组失败: {error}"));
        count > 0
    }

    /// 组是否仍按主键存在（竞态用例必须按 id 观测）。
    pub async fn group_exists_by_id(app: &BuiltApp, group_id: i64) -> bool {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM permission_group WHERE id = ?")
            .bind(group_id)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("统计权限组失败: {error}"));
        count > 0
    }

    /// 一个组当前的成员行数。
    pub async fn member_rows_of_group(app: &BuiltApp, group_id: i64) -> u64 {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_group WHERE group_id = ?")
            .bind(group_id)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("统计组成员失败: {error}"));
        u64::try_from(count).unwrap_or_else(|error| panic!("成员行数为负: {error}"))
    }

    /// 走真实成员 Action 把一个用户加入组（失败即 panic：调用方断言的是成功路径）。
    pub async fn add_member(app: &BuiltApp, operator: &Admin, group_id: i64, user_id: i64) {
        let response = member_dispatch(app, operator, "add_group_member", group_id, user_id)
            .await
            .unwrap_or_else(|error| panic!("把用户 {user_id} 加入组 {group_id} 失败: {error}"));
        assert_eq!(response.code, 0, "加成员必须成功: {}", response.message);
    }

    /// 尝试加成员并折算为 HTTP 状态码。
    ///
    /// 200 表示插入成功（或幂等重复）；403 表示撞上 spec §8.1 的防自提权判定；
    /// 404 表示组已被并发删除——应用层的前置读取与外键兜底都折算到这一支。
    pub async fn add_member_status(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        user_id: i64,
    ) -> u16 {
        match member_dispatch(app, operator, "add_group_member", group_id, user_id).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试移出成员并折算为 HTTP 状态码（400 为最后管理员守卫的拒绝）。
    pub async fn remove_member_status(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        user_id: i64,
    ) -> u16 {
        match member_dispatch(app, operator, "remove_group_member", group_id, user_id).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 用户当前是否属于该组。
    pub async fn is_member(app: &BuiltApp, group_id: i64, user_id: i64) -> bool {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_group WHERE group_id = ? AND user_id = ?",
        )
        .bind(group_id)
        .bind(user_id)
        .fetch_one(pool_of(app))
        .await
        .unwrap_or_else(|error| panic!("查询成员关系失败: {error}"));
        count > 0
    }

    /// 内置全权组的 id（由引导流程幂等创建）。
    pub async fn system_admin_group_id(app: &BuiltApp) -> i64 {
        sqlx::query_scalar("SELECT id FROM permission_group WHERE group_key = ?")
            .bind(SYSTEM_ADMIN_GROUP_KEY)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("读取内置全权组 ID 失败: {error}"))
    }

    /// 注册一个只被直授 `permission` 的账号并登录取令牌。
    ///
    /// 「只」是这些用例的全部意义：账号除这一个权限外什么都没有，因此它一旦让自身
    /// 有效权限变大，就必定是自提权。
    pub async fn grant_only(
        app: &BuiltApp,
        admin: &Admin,
        username: &str,
        permission: &str,
    ) -> Admin {
        let email = format!("{username}@example.com");
        let user_id = register_with_code(app, username, &email)
            .await
            .unwrap_or_else(|error| panic!("注册 {username} 失败: {error}"));
        step_up_dispatch(
            app,
            "access.grants",
            "grant_permission",
            json!({ "user_id": user_id, "permission": permission }),
            admin,
        )
        .await
        .unwrap_or_else(|error| panic!("授予 {username} 权限 {permission} 失败: {error}"));
        Admin {
            username: username.to_string(),
            user_id,
            // 直授会递增授权版本，令牌必须在授权**之后**签发才带得上这个权限。
            token: login_as(app, username).await,
        }
    }

    /// 再引导一名系统管理员：注册后经真实成员 Action 加入内置全权组。
    ///
    /// 入组会递增该账号的授权版本，因此令牌在入组**之后**签发——否则它一出生就是
    /// 过期的，用例会停在 401。
    pub async fn add_second_admin(app: &BuiltApp, first: &Admin, username: &str) -> Admin {
        let email = format!("{username}@example.com");
        let user_id = register_with_code(app, username, &email)
            .await
            .unwrap_or_else(|error| panic!("注册 {username} 失败: {error}"));
        let group_id = system_admin_group_id(app).await;
        let status = add_member_status(app, first, group_id, user_id).await;
        assert_eq!(status, 200, "把 {username} 加入全权组必须成功");
        Admin {
            username: username.to_string(),
            user_id,
            token: login_as(app, username).await,
        }
    }

    /// 用同一账号重新签发令牌。
    ///
    /// 组成员变更会递增该成员的授权版本、让旧令牌立即失效；用例若要在入组之后继续
    /// 以该成员身份调用接口，就必须先换发令牌——这正是「旧 Token 失效」的另一面，
    /// 不是给测试开的后门。
    pub async fn relogin(app: &BuiltApp, admin: &Admin) -> Admin {
        let token = login_as(app, &admin.username).await;
        Admin {
            token,
            ..admin.clone()
        }
    }

    /// 驱动一个组成员写 Action（普通认证请求）。
    ///
    /// 与组条目同例：成员加/移出**不在** `step_up_targets()` 登记内（那三个是
    /// 建/改/删组），因此不能走 `step_up_dispatch`——那会要求这些 Action 挂 Step-up
    /// 守卫，而守卫清单本身由 `access.groups` 的单测钉死。
    async fn member_dispatch(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        user_id: i64,
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", operator.token);
        dispatch(
            app,
            "access.groups",
            action,
            json!({ "group_id": group_id, "user_id": user_id }),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
    }

    fn pool_of(app: &BuiltApp) -> &sqlx::MySqlPool {
        app.tools()
            .mysql()
            .unwrap_or_else(|error| panic!("测试应用必须配置 MySQL: {error}"))
            .pool()
    }

    /// 驱动一个受 Step-up 保护的写 Action：先无 proof 触发 challenge，用密码重认证
    /// 换一次性 proof，再带 proof 重试同一次调用。
    ///
    /// 第一次调用若直接成功，说明 Step-up 守卫根本没挂上——那是安全回归，必须让用例
    /// 失败，因此这里把它折算成 `ConfigError`（500）而不是放行。
    async fn step_up_dispatch(
        app: &BuiltApp,
        module: &str,
        action: &str,
        body: Value,
        admin: &Admin,
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", admin.token);
        match dispatch(
            app,
            module,
            action,
            body.clone(),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => {
                let completed = dispatch(
                    app,
                    "account.user",
                    "step_up_complete",
                    json!({
                        "challenge": challenge.challenge,
                        "credentials": { "username": admin.username, "password": PASSWORD },
                    }),
                    &[],
                    PEER_PORT,
                )
                .await?;
                let proof = completed
                    .data
                    .as_ref()
                    .and_then(|data| data["proof"].as_str())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        BaseError::ConfigError("Step-up 完成响应缺少 proof".to_string())
                    })?;
                dispatch(
                    app,
                    module,
                    action,
                    body,
                    &[
                        ("authorization", authorization.as_str()),
                        (STEP_UP_PROOF_HEADER, proof.as_str()),
                    ],
                    PEER_PORT,
                )
                .await
            }
            Ok(response) => Err(BaseError::ConfigError(format!(
                "{action} 未受 Step-up 保护：无 proof 也成功了（{}）",
                response.message
            ))),
            Err(other) => Err(other),
        }
    }

    /// 把 Action 错误折算为 HTTP 状态码。
    ///
    /// 框架的映射函数是传输层私有实现，集成测试只能按同一套分类重算：先看有专属
    /// 状态的变体，其余按 `ErrorCategory` 归并；未覆盖的类别一律 500，让预期外错误
    /// 直接表现为断言失败而不是被静默吞掉。
    fn http_status(error: &BaseError) -> u16 {
        match error {
            BaseError::StepUpRequired(_) => 428,
            BaseError::PermissionDenied(_) | BaseError::FieldPermissionDenied(_, _, _) => 403,
            BaseError::Unauthorized(_) => 401,
            BaseError::RecordNotFound(_) | BaseError::UserNotFound(_) => 404,
            BaseError::ParamInvalid(_, _) => 400,
            other => match other.category() {
                ErrorCategory::Client => 400,
                ErrorCategory::Auth => 401,
                ErrorCategory::NotFound => 404,
                ErrorCategory::Conflict => 409,
                ErrorCategory::Transient => 503,
                _ => 500,
            },
        }
    }

    // -----------------------------------------------------------------------
    // Task 11 起：组条目夹具。
    // -----------------------------------------------------------------------

    /// 组成员上限的生产口径（`access/domain/groups/repository.rs` 的
    /// `MAX_GROUP_MEMBERS`）。
    ///
    /// 集成测试是独立 crate，读不到应用侧的 `pub(crate)` 常量，因此在此镜像一份；
    /// 生产常量另有单测钉死取值（`max_group_members_is_a_named_constant`），
    /// 它变更时会失败并提示同步这里。
    pub const MAX_GROUP_MEMBERS: usize = 200;

    /// 读取用户当前的授权版本；它是 Access Token 新鲜度的唯一事实源。
    pub async fn authz_version_of(app: &BuiltApp, user_id: i64) -> i64 {
        sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("读取用户 {user_id} 授权版本失败: {error}"))
    }

    /// 一个组当前的权限条目行数。
    pub async fn item_rows_of_group(app: &BuiltApp, group_id: i64) -> u64 {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM permission_group_item WHERE group_id = ?")
                .bind(group_id)
                .fetch_one(pool_of(app))
                .await
                .unwrap_or_else(|error| panic!("统计组条目失败: {error}"));
        u64::try_from(count).unwrap_or_else(|error| panic!("条目行数为负: {error}"))
    }

    /// 直写一条组条目行（用于构造目录里已不存在的孤儿条目）。
    pub async fn insert_item_row(app: &BuiltApp, group_id: i64, permission: &str, granted_by: i64) {
        sqlx::query(
            "INSERT INTO permission_group_item (group_id, permission, granted_by, occurred_at) \
             VALUES (?, ?, ?, UNIX_TIMESTAMP())",
        )
        .bind(group_id)
        .bind(permission)
        .bind(granted_by)
        .execute(pool_of(app))
        .await
        .unwrap_or_else(|error| panic!("直写组条目 {permission} 失败: {error}"));
    }

    /// 用已注册账号登录换取 Access Token。
    ///
    /// 「组事实变更后旧 Token 失效」必须拿**变更前**签发的那个 Token 去验证，
    /// 所以登录取令牌要能单独调用——`register_with_code` 只注册不登录。
    pub async fn login_as(app: &BuiltApp, username: &str) -> String {
        login(app, username, PEER_PORT)
            .await
            .unwrap_or_else(|error| panic!("登录 {username} 失败: {error}"))
    }

    /// 该 Access Token 是否已被授权版本水位线判定为过期。
    ///
    /// 探针用无权限要求的 `account.user.me`：它只经过认证中间件，因此「被拒」只
    /// 可能来自凭据新鲜度校验，不会与权限判定（403）或参数校验（400）混淆；
    /// 其它错误一律 panic，避免把预期外故障读成「过期」。
    pub async fn member_token_is_stale(app: &BuiltApp, token: &str) -> bool {
        let authorization = format!("Bearer {token}");
        match dispatch(
            app,
            "account.user",
            "me",
            json!({}),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
        {
            Ok(_) => false,
            Err(BaseError::AuthorizationStale) => true,
            Err(other) => panic!("Token 新鲜度探针返回预期外错误: {other}"),
        }
    }

    /// 驱动一个组条目写 Action（普通认证请求）。
    ///
    /// 条目加/移除**不在** `step_up_targets()` 登记内（Task 10 只登记建/改/删三个），
    /// 因此这里不能走 `step_up_dispatch`——那会要求这些 Action 挂 Step-up 守卫。
    async fn group_item_dispatch(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        permission: &str,
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", operator.token);
        dispatch(
            app,
            "access.groups",
            action,
            json!({ "group_id": group_id, "permission": permission }),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
    }

    /// 向组追加一条权限；失败即 panic（调用方断言的是成功路径）。
    pub async fn add_group_item(app: &BuiltApp, operator: &Admin, group_id: i64, permission: &str) {
        group_item_dispatch(app, operator, "add_group_item", group_id, permission)
            .await
            .unwrap_or_else(|error| panic!("向组 {group_id} 加权限 {permission} 失败: {error}"));
    }

    /// 向组追加一条权限并折算为 HTTP 状态码。
    pub async fn add_group_item_status(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        permission: &str,
    ) -> u16 {
        match group_item_dispatch(app, operator, "add_group_item", group_id, permission).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 从组移除一条权限并折算为 HTTP 状态码。
    pub async fn remove_group_item_status(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        permission: &str,
    ) -> u16 {
        match group_item_dispatch(app, operator, "remove_group_item", group_id, permission).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 直写成员行把组补到 `total` 名成员，返回落库后**读回**的成员数。
    ///
    /// 成员上限用例需要 200+ 名成员，走真实注册路径要跑 200 次 argon2 摘要，因此
    /// 与既有夹具同例直接落库：占位账号按 `users` 表的列口径写入最小必需列。
    pub async fn seed_members_directly(app: &BuiltApp, group_id: i64, total: usize) -> usize {
        let existing = member_rows_of_group(app, group_id).await;
        let granted_by = created_by_of_group(app, group_id).await;
        for index in existing..u64::try_from(total).unwrap_or(u64::MAX) {
            let username = format!("bulk_member_{group_id}_{index:04}");
            let user_id = insert_placeholder_user(app, &username).await;
            sqlx::query(
                "INSERT INTO user_group (user_id, group_id, granted_by, occurred_at) \
                 VALUES (?, ?, ?, UNIX_TIMESTAMP())",
            )
            .bind(user_id)
            .bind(group_id)
            .bind(granted_by)
            .execute(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("直写组成员 {username} 失败: {error}"));
        }
        usize::try_from(member_rows_of_group(app, group_id).await)
            .unwrap_or_else(|error| panic!("成员数超出 usize: {error}"))
    }

    /// 直写一个占位账号，返回其主键。
    ///
    /// 时间戳列由 ORM 在插入时填充、表声明里没有数据库默认值，因此必须显式给出
    /// （否则严格模式报 1364）；邮箱留空——`users` 的 CHECK 要求 `email` 与
    /// `email_verified_at` 同为空或同非空。
    async fn insert_placeholder_user(app: &BuiltApp, username: &str) -> i64 {
        sqlx::query(
            "INSERT INTO users \
             (username, password_hash, status, authz_version, credential_version, created_at, updated_at) \
             VALUES (?, ?, 'active', 1, 0, UNIX_TIMESTAMP(), UNIX_TIMESTAMP())",
        )
        .bind(username)
        .bind("integration-fixture-placeholder-hash")
        .execute(pool_of(app))
        .await
        .unwrap_or_else(|error| panic!("直写占位账号 {username} 失败: {error}"));
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(username)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("回查占位账号 {username} 失败: {error}"))
    }

    async fn created_by_of_group(app: &BuiltApp, group_id: i64) -> i64 {
        sqlx::query_scalar("SELECT created_by FROM permission_group WHERE id = ?")
            .bind(group_id)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("读取权限组 {group_id} 创建人失败: {error}"))
    }
}

/// spec §8.3 与 Review Focus 3：删除仍有成员的组必须被拒，且不产生 500。
///
/// 计划此处断言 409（`BaseError::Conflict`）。框架没有 `Conflict` 变体，且设计 §9.3
/// 同时要求「不为个别用例扩展框架错误类型」，因此拒绝只能落成既有的 `ParamInvalid`
/// （400）——与 Task 6 的成员上限拒绝同一取舍。断言要钉住的事实不变：必须被拒、
/// 绝不能是 5xx、组必须仍然存在。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn deleting_a_group_with_members_is_rejected() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "ops", "运维").await;
    let user_id = harness::register_with_code(&app, "member1", "member1@example.com")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    harness::add_member(&app, &admin, group_id, user_id).await;

    let status = harness::delete_group_status(&app, &admin, group_id).await;
    assert_eq!(
        status, 400,
        "组内非空必须被拒（ParamInvalid→400），绝不能是 500"
    );
    assert!(harness::group_exists(&app, "ops").await, "拒绝后组必须仍在");
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        1,
        "拒绝不得顺手清掉成员行"
    );
}

/// Review Focus 3：删除与加成员并发时，结果必须收敛到「删成功」或「冲突」。
///
/// 允许的状态集把计划里的 409 换成 400：删组被拒的原因（组内非空）在框架里是
/// `ParamInvalid`（见上一个用例的说明）。真正被钉住的契约不变——任何一侧都不得
/// 出现 5xx，且组一旦消失就不得残留成员行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn delete_races_add_member_without_orphans_or_500() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;

    for round in 0..5 {
        let group_id = harness::create_group(&app, &admin, &format!("race{round}"), "竞态").await;
        let user_id = harness::register_with_code(
            &app,
            &format!("racer{round}"),
            &format!("racer{round}@example.com"),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"));

        let (delete_result, add_result) = tokio::join!(
            harness::delete_group_status(&app, &admin, group_id),
            harness::add_member_status(&app, &admin, group_id, user_id),
        );

        for status in [delete_result, add_result] {
            assert!(
                status == 200 || status == 400 || status == 404,
                "只允许 200/400/404，实际 {status}"
            );
        }
        // 无论谁赢，都不能留下悬空成员行：组不存在则成员行必须为 0。
        if !harness::group_exists_by_id(&app, group_id).await {
            assert_eq!(
                harness::member_rows_of_group(&app, group_id).await,
                0,
                "组已删除时不得残留成员行（外键 RESTRICT 必须兜住）"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Task 11 起的组条目用例：加/移除权限，含扇出失效与成员上限边界。
// ---------------------------------------------------------------------------

/// spec §6.3：组权限变更必须让全部成员的 Token 失效。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn adding_a_group_permission_invalidates_every_member() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "ops", "运维").await;
    let member = harness::register_with_code(&app, "ops1", "ops1@example.com")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    harness::add_member(&app, &admin, group_id, member).await;

    // 变更**前**签发的 Token：它正是本次扇出失效要判死的对象。
    let issued_before = harness::login_as(&app, "ops1").await;
    let before = harness::authz_version_of(&app, member).await;
    harness::add_group_item(&app, &admin, group_id, "access.grants.read").await;
    let after = harness::authz_version_of(&app, member).await;

    assert!(after > before, "组权限变更必须递增成员授权版本");
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        1,
        "权限必须真的落进条目表"
    );
    assert!(
        harness::member_token_is_stale(&app, &issued_before).await,
        "旧 Token 必须被判定为过期"
    );
    // 反面对照：变更后重新签发的 Token 必须有效。没有这一条，「探针恒为 stale」
    // 与「旧 Token 恰好过期」无法区分。
    let issued_after = harness::login_as(&app, "ops1").await;
    assert!(
        !harness::member_token_is_stale(&app, &issued_after).await,
        "变更后重新签发的 Token 必须仍然有效"
    );
}

/// Review Focus 4：成员数上限的边界行为必须可预测。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn group_permission_change_respects_the_member_limit() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 极限测试用专用常量；若 MAX_GROUP_MEMBERS 变更，此测试与常量一并更新。
    let group_id = harness::create_group(&app, &admin, "big", "大组").await;

    let at_limit = harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS).await;
    assert_eq!(at_limit, harness::MAX_GROUP_MEMBERS);
    harness::add_group_item(&app, &admin, group_id, "access.grants.read").await;

    let over_limit =
        harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS + 1).await;
    assert_eq!(over_limit, harness::MAX_GROUP_MEMBERS + 1);
    let status = harness::add_group_item_status(&app, &admin, group_id, "account.users.read").await;
    // 计划此处写 409（设计 §9.3 的 `Conflict`）：`yang_base::BaseError` 没有 `Conflict`
    // 变体，且设计同节要求「不为个别用例扩展框架错误类型」，因此超限拒绝落成既有的
    // `ParamInvalid`（400）——与 Task 6 的 `ensure_member_limit`、Task 10 的删组拒绝
    // 同一取舍。被钉住的契约不变：必须被明确拒绝，且不得留下任何写结果。
    assert_eq!(status, 400, "超过上限必须明确报错");
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        1,
        "超限拒绝不得写入新条目（只应剩上限内的那一条）"
    );
}

/// 移除组权限：同样扇出失效、同样幂等，且必须能清理目录之外的孤儿条目。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn removing_a_group_permission_invalidates_members_and_is_idempotent() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "ops", "运维").await;
    let member = harness::register_with_code(&app, "ops2", "ops2@example.com")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    harness::add_member(&app, &admin, group_id, member).await;
    harness::add_group_item(&app, &admin, group_id, "access.grants.read").await;

    let before = harness::authz_version_of(&app, member).await;
    assert_eq!(
        harness::remove_group_item_status(&app, &admin, group_id, "access.grants.read").await,
        200,
        "移除组内已有权限必须成功"
    );
    let after = harness::authz_version_of(&app, member).await;
    assert!(after > before, "移除组权限必须递增成员授权版本");
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        0,
        "条目行必须被删除"
    );

    // 幂等：重复移除仍然成功，但不再改变任何事实（版本不得再增）。
    assert_eq!(
        harness::remove_group_item_status(&app, &admin, group_id, "access.grants.read").await,
        200,
        "重复移除必须幂等成功"
    );
    assert_eq!(
        harness::authz_version_of(&app, member).await,
        after,
        "幂等移除不得再递增授权版本"
    );

    // 反向宽容语义：已不在权限目录里的权限也必须能清理，否则孤儿条目永远清不掉
    // （对齐 `revoke_permission.rs` 的不做 `ensure_declared`）。
    harness::insert_item_row(&app, group_id, "removed.module.act", admin.user_id).await;
    assert_eq!(harness::item_rows_of_group(&app, group_id).await, 1);
    assert_eq!(
        harness::remove_group_item_status(&app, &admin, group_id, "removed.module.act").await,
        200,
        "目录之外的孤儿条目必须能被清理"
    );
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        0,
        "孤儿条目必须被真的删除"
    );
}

/// 加条目必须 fail-closed：目录里没有声明的权限一律拒绝，且不留下任何写结果。
///
/// 这是 Review Focus 1 在本任务可达的那一半——「目录未安装」态由
/// `PermissionCatalogHandle` 的单测覆盖（集成路径的三种装配入口都会安装目录，
/// 造不出未安装的应用）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn adding_an_undeclared_permission_is_rejected() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "ops", "运维").await;

    let status =
        harness::add_group_item_status(&app, &admin, group_id, "nonexistent.module.act").await;
    assert_eq!(status, 400, "未声明的权限必须 fail-closed 拒绝");
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        0,
        "拒绝不得留下条目行"
    );
}

// ---------------------------------------------------------------------------
// Task 12 起的组成员用例：加/移出成员、两条自提权路径与最后管理员守卫。
// ---------------------------------------------------------------------------

/// spec §8.1 路径一：给自己加一个权限超集的组必须被拒。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn user_cannot_add_themselves_to_a_group_granting_more_than_they_hold() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // operator 只被授予 access.groups.write，不含 access.grants.write。
    let operator = harness::grant_only(&app, &admin, "operator", "access.groups.write").await;
    let powerful = harness::create_group(&app, &admin, "powerful", "高权").await;
    harness::add_group_item(&app, &admin, powerful, "access.grants.write").await;

    let status = harness::add_member_status(&app, &operator, powerful, operator.user_id).await;
    assert_eq!(status, 403, "自提权必须被拒绝");
    assert!(!harness::is_member(&app, powerful, operator.user_id).await);
}

/// spec §8.1 路径二：给自己已属于的组加一条自己没有的权限，同样必须被拒。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn user_cannot_add_a_permission_to_their_own_group_beyond_their_holdings() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let operator = harness::grant_only(&app, &admin, "operator2", "access.groups.write").await;
    let own = harness::create_group(&app, &admin, "own", "自属组").await;
    harness::add_group_item(&app, &admin, own, "access.groups.write").await;
    harness::add_member(&app, &admin, own, operator.user_id).await;
    // 入组递增了 operator 的授权版本，入组前签发的令牌已失效；换发之后才轮到
    // §8.1 的子集判定——否则用例会停在 401，根本测不到提权拒绝。
    let operator = harness::relogin(&app, &operator).await;

    let status = harness::add_group_item_status(&app, &operator, own, "access.grants.write").await;
    assert_eq!(status, 403, "给自己所属组加超集权限同样是自提权");
    assert_eq!(
        harness::item_rows_of_group(&app, own).await,
        1,
        "被拒的自提权不得留下条目行"
    );
}

/// Review Focus 2：管理员把自己移出全权组（非最后一名）必须成功。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn admin_can_remove_themselves_when_another_admin_remains() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;
    let group_id = harness::system_admin_group_id(&app).await;

    let version_before = harness::authz_version_of(&app, second.user_id).await;
    let status = harness::remove_member_status(&app, &second, group_id, second.user_id).await;
    assert_eq!(status, 200, "非最后一名管理员可以退出全权组");
    assert!(!harness::is_member(&app, group_id, second.user_id).await);
    // 事实层：退出必须立刻递增该账号的授权版本（spec §6.3 的扇出失效）。
    assert!(
        harness::authz_version_of(&app, second.user_id).await > version_before,
        "移出组成员必须递增该成员的授权版本"
    );

    // 退出后该账号失去全部权限（旧 Token 失效）。
    //
    // 这一条为什么必须等：校验器的快速路径在 Redis 版本与 claims 相同就直接放行
    // （`request_validator.rs`），而 Redis 里的版本由 Outbox worker 异步刷新——本夹具
    // 不启动 worker，且本用例自己那次请求刚把旧版本回填进了缓存。因此要观测到
    // `AuthorizationStale`，必须等这条被写成旧值的键自然过期。ADR
    // （docs/architecture/authorization-freshness-adr.md）把「数据库提交后最坏 5 秒
    // 陈旧窗口」列为明确接受的代价，这里等的是同一件事，不是给断言放水：若移出没有
    // 递增版本，回查主库时会以「版本相同」放行，本断言照样失败。
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    assert!(
        harness::member_token_is_stale(&app, &second.token).await,
        "移出全权组后旧 Token 必须失效"
    );
}

/// spec §8.2 在组成员接口上的落点：移出全权组成员后系统必须仍有至少一名 active 管理员。
///
/// 计划此处断言 409（设计 §9.3 的 `Conflict`）。`yang_base::BaseError` 没有该变体，
/// 且设计同节要求「不为个别用例扩展框架错误类型」，因此拒绝落成既有的 `ParamInvalid`
/// （400）——与本文件里「删组」「成员上限」两条守卫同一取舍。断言要钉住的事实不变：
/// 必须被拒、绝不能是 5xx、拒绝后成员关系必须原封不动。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn the_last_system_admin_cannot_be_removed_from_the_admin_group() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::system_admin_group_id(&app).await;

    let status = harness::remove_member_status(&app, &admin, group_id, admin.user_id).await;
    assert_eq!(
        status, 400,
        "最后一名管理员必须被拒（ParamInvalid→400），绝不能是 500"
    );
    assert!(
        harness::is_member(&app, group_id, admin.user_id).await,
        "拒绝不得顺手删掉成员行"
    );
}
