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

use common::{take_registration_code, RegistrationEmailToolsExt};

const PASSWORD: &str = "correct-horse-battery-staple";
const GROUP_READ_PERMISSION: &str = "access.grants.read";
const SYSTEM_ADMIN_GROUP_KEY: &str = "system_admin";
/// 管理员等价权限的期望集合（G2）。
///
/// 刻意在测试侧**独立复述**一遍清单，而不是从应用侧读回来：这样「清单里每条仍是
/// 当前 Catalog 已声明的权限」才有独立的判据——权限改名后清单会静默失效，只有这份
/// 与代码侧清单分离的期望值才能把它钉红。生产清单见
/// `src/addon/access/domain/sensitive_permissions.rs`。
const ADMIN_EQUIVALENT_PERMISSIONS: [&str; 3] = [
    "account.users.reset_credentials",
    "feishu.datasource.secret",
    "feishu.datasource.write",
];

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
///
/// `authz_grant` 没有外键、DROP `users` 不会带走它，而本文件的用例会直授权限并
/// 断言「某用户名下剩几行」——漏删它会让上一轮运行的行按 `user_id` 撞进下一个
/// 用例（自增主键从 1 重新开始），断言与权限集合都会被污染。
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
    dispatch_with_path(app, module, action, body, headers, peer_port, &[]).await
}

/// 同 [`dispatch`]，额外携带路径参数。
///
/// 路径参数必须走 `request.path_params`：`params!` 生成的 `decode` 只对
/// `source = body` 的字段读请求体，把路径参数写进 JSON body 会被判为缺参
/// （`ParamInvalid("input", "missing field ...")`）。
async fn dispatch_with_path(
    app: &BuiltApp,
    module: &str,
    action: &str,
    body: Value,
    headers: &[(&str, &str)],
    peer_port: u16,
    path_params: &[(&str, &str)],
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    for (name, value) in path_params {
        request = request.path_param(*name, *value);
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

/// 保留权限键的期望集合（生产镜像）。
///
/// `access.groups` 域的全局读写键不再由任何 Action 以 `.permissions(...)` 声明，
/// 但仍是目录条目：可授予、可进入全权组 Token claims、由 handler 内判定消费。
/// 生产清单见 `src/addon/access/domain/permission_catalog.rs` 的
/// `RETAINED_PERMISSION_KEYS`。刻意在测试侧独立复述：与
/// [`ADMIN_EQUIVALENT_PERMISSIONS`] 同款理由——清单增删时全权组期望值才能跟着变红。
const RETAINED_PERMISSION_KEYS: [&str; 2] = ["access.groups.read", "access.groups.write"];

/// 冻结 Catalog 声明的全部权限——内置全权组的期望值。
///
/// 在测试侧独立重算目录（而不是复用 `project_permissions`），使断言真的能
/// 抓住「组解析漏权限 / 多权限」的实现错误。Action 声明与模块默认权限之外，
/// 必须并入保留权限键（[`RETAINED_PERMISSION_KEYS`]）：它们虽无 Action 声明，
/// 但作为目录条目可授予、并会进入全权组 Token claims。
fn declared_permissions(app: &BuiltApp) -> Vec<String> {
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for retained in RETAINED_PERMISSION_KEYS {
        declared.insert(retained.to_string());
    }
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
/// 把「注册账号 → 登录取令牌 → 组写操作 → 直接观测数据库事实」
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
        build_application, connect_test_database, connect_test_redis, dispatch, dispatch_with_path,
        login, reset_database, reset_redis, PASSWORD, SYSTEM_ADMIN_GROUP_KEY,
    };
    use anyhow::{ensure, Context};
    use serde_json::{json, Value};
    use yang_base::action::ApiResponse;
    use yang_base::definition::BuiltApp;
    use yang_base::error::ErrorCategory;
    use yang_base::BaseError;

    /// 夹具固定的对端端口：注册限流按 IP 计数，阈值已放大到用例不会触发。
    const PEER_PORT: u16 = 52_400;

    /// 管理员身份：用户名、用户 ID 与令牌。
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

    /// 建组并返回新组 ID。
    pub async fn create_group(app: &BuiltApp, admin: &Admin, group_key: &str, title: &str) -> i64 {
        let response = authed_dispatch(
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

    /// 尝试建组并折算为（HTTP 状态码, 消息）。
    ///
    /// 与 [`create_group`] 的唯一差别是**不 panic**：保留 key 用例要观测的是「被拒」，
    /// 而不是让夹具直接把用例炸掉。成功时消息是成功文案。
    pub async fn create_group_outcome(
        app: &BuiltApp,
        operator: &Admin,
        group_key: &str,
        title: &str,
    ) -> (u16, String) {
        match authed_dispatch(
            app,
            "access.groups",
            "create_group",
            json!({ "group_key": group_key, "title": title }),
            operator,
        )
        .await
        {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 按 `group_key` 统计成员行数（保留 key 用例：被拒的创建不得留下任何成员行）。
    ///
    /// 走 `user_group ⋈ permission_group` 而不是单个 group_id：保留 key 用例要断言的
    /// 恰恰是「没有以该 key 建出的组，因此也不该有挂在它名下的成员行」。
    pub async fn member_rows_of_group_key(app: &BuiltApp, group_key: &str) -> u64 {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_group ug JOIN permission_group g ON ug.group_id = g.id \
             WHERE g.group_key = ?",
        )
        .bind(group_key)
        .fetch_one(pool_of(app))
        .await
        .unwrap_or_else(|error| panic!("按 key {group_key} 统计成员行失败: {error}"));
        u64::try_from(count).unwrap_or_else(|error| panic!("成员行数为负: {error}"))
    }

    /// 直接从库里抹掉内置全权组（条目行 → 成员行 → 组行），复现设计 §7.3 的灾备态。
    ///
    /// 这是保留 key 唯一可能被公开创建路径抢占的状态：正常引导后该 key 已在库中，重复
    /// 建组只会撞唯一键，测不到「创建期拒绝」这条防线。触发真实灾备流程（手工重放运维
    /// SQL）对集成测试过重，且会污染被测事实，故按灾备模板的效果直接落库——
    /// 夹具直写是本文件的既定例外（见文件头与 `insert_group`）。
    ///
    /// 删除顺序遵从外键 RESTRICT：先条目、再成员，最后组行。
    pub async fn drop_builtin_admin_group(app: &BuiltApp) {
        let group_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM permission_group WHERE group_key = ?")
                .bind(SYSTEM_ADMIN_GROUP_KEY)
                .fetch_all(pool_of(app))
                .await
                .unwrap_or_else(|error| panic!("读取内置全权组失败: {error}"));
        for group_id in group_ids {
            for table in ["permission_group_item", "user_group"] {
                sqlx::query(&format!("DELETE FROM {table} WHERE group_id = ?"))
                    .bind(group_id)
                    .execute(pool_of(app))
                    .await
                    .unwrap_or_else(|error| panic!("清理内置组的 {table} 行失败: {error}"));
            }
            sqlx::query("DELETE FROM permission_group WHERE id = ?")
                .bind(group_id)
                .execute(pool_of(app))
                .await
                .unwrap_or_else(|error| panic!("删除内置全权组 {group_id} 失败: {error}"));
        }
    }

    /// 删组并折算为 HTTP 状态码（成功为 200，其余由用例断言）。
    pub async fn delete_group_status(app: &BuiltApp, admin: &Admin, group_id: i64) -> u16 {
        match authed_dispatch(
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
        let response = member_mutation(app, operator, "add_group_member", group_id, user_id)
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
        match member_mutation(app, operator, "add_group_member", group_id, user_id).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试加成员，返回（HTTP 状态码，拒绝时的错误消息）。
    ///
    /// 成员上限用例必须同时钉住「被拒」与「拒绝理由说得清」两件事：只断言状态码
    /// 无法区分「撞上成员上限」与「参数校验顺手拦下」，而调用方要照消息办事。
    /// 成功时消息是成功文案（用例只会在拒绝路径上读它）。
    pub async fn add_member_outcome(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        user_id: i64,
    ) -> (u16, String) {
        match member_mutation(app, operator, "add_group_member", group_id, user_id).await {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 尝试移出成员并折算为 HTTP 状态码（400 为最后管理员守卫的拒绝）。
    pub async fn remove_member_status(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        user_id: i64,
    ) -> u16 {
        match member_mutation(app, operator, "remove_group_member", group_id, user_id).await {
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
        authed_dispatch(
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

    /// 给已存在的账号补一条直授权限，不重新签发令牌。
    ///
    /// [`grant_only`] 只覆盖「恰好一条权限」的夹具；锁序用例需要操作者既持
    /// `access.groups.write`（否则调用不了组条目 Action），又持一条它准备写进组的
    /// 权限（否则会撞上 §8.1 的子集校验），因此需要这条补充授权的路径。
    pub async fn grant_permission(app: &BuiltApp, admin: &Admin, user_id: i64, permission: &str) {
        authed_dispatch(
            app,
            "access.grants",
            "grant_permission",
            json!({ "user_id": user_id, "permission": permission }),
            admin,
        )
        .await
        .unwrap_or_else(|error| panic!("授予账号 {user_id} 权限 {permission} 失败: {error}"));
    }

    /// 尝试授予权限并折算为 HTTP 状态码（403 = 撞上管理员等价闸门）。
    ///
    /// 与 [`grant_permission`] 的唯一差别是**不 panic**：管理员等价权限的闸门用例
    /// 刻意用一个非全权组成员当操作者，需要观测「被拒」而不是让夹具直接炸掉。
    pub async fn grant_permission_status(
        app: &BuiltApp,
        operator: &Admin,
        user_id: i64,
        permission: &str,
    ) -> u16 {
        match authed_dispatch(
            app,
            "access.grants",
            "grant_permission",
            json!({ "user_id": user_id, "permission": permission }),
            operator,
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 授予单条直授权限并返回完整响应（`expires_at` 可缺省；续期用例观测 `changed`）。
    ///
    /// [`grant_permission`] / [`grant_permission_status`] 都丢弃或折算掉了响应体，
    /// 续期用例需要读 `data.changed` 区分「续期（changed=true）」与「幂等跳过
    /// （changed=false）」，因此需要这条保响应的夹具。
    pub async fn grant_permission_response(
        app: &BuiltApp,
        operator: &Admin,
        user_id: i64,
        permission: &str,
        expires_at: Option<i64>,
    ) -> Result<ApiResponse, BaseError> {
        let body = match expires_at {
            Some(expires_at) => json!({
                "user_id": user_id,
                "permission": permission,
                "expires_at": expires_at,
            }),
            None => json!({ "user_id": user_id, "permission": permission }),
        };
        authed_dispatch(app, "access.grants", "grant_permission", body, operator).await
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

    /// 组成员写 Action 的请求体构造。
    fn member_body(group_id: i64, user_id: i64) -> Value {
        json!({ "group_id": group_id, "user_id": user_id })
    }

    /// 驱动一个组成员写 Action，返回原始结果。
    async fn member_mutation(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        user_id: i64,
    ) -> Result<ApiResponse, BaseError> {
        authed_dispatch(
            app,
            "access.groups",
            action,
            member_body(group_id, user_id),
            operator,
        )
        .await
    }

    /// 驱动一个组成员写 Action，折算为（状态码, 消息）。
    ///
    /// 并发用例直接经这条路径投出请求，Barrier 对齐请求起点，事务临界段的重叠
    /// 由真库的独立连接与行锁保证。
    pub async fn member_mutation_outcome(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        user_id: i64,
    ) -> (u16, String) {
        match member_mutation(app, operator, action, group_id, user_id).await {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    fn pool_of(app: &BuiltApp) -> &sqlx::MySqlPool {
        app.tools()
            .mysql()
            .unwrap_or_else(|error| panic!("测试应用必须配置 MySQL: {error}"))
            .pool()
    }

    /// 用登录令牌驱动一个写 Action，返回原始结果。
    async fn authed_dispatch(
        app: &BuiltApp,
        module: &str,
        action: &str,
        body: Value,
        admin: &Admin,
    ) -> Result<ApiResponse, BaseError> {
        authed_dispatch_with_path(app, module, action, body, admin, &[]).await
    }

    /// 同 [`authed_dispatch`]，额外携带路径参数（如 `/api/v1/users/{id}/disable`）。
    async fn authed_dispatch_with_path(
        app: &BuiltApp,
        module: &str,
        action: &str,
        body: Value,
        admin: &Admin,
        path_params: &[(&str, &str)],
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", admin.token);
        dispatch_with_path(
            app,
            module,
            action,
            body,
            &[("authorization", authorization.as_str())],
            PEER_PORT,
            path_params,
        )
        .await
    }

    /// 把 Action 错误折算为 HTTP 状态码。
    ///
    /// 框架的映射函数是传输层私有实现，集成测试只能按同一套分类重算：先看有专属
    /// 状态的变体，其余按 `ErrorCategory` 归并；未覆盖的类别一律 500，让预期外错误
    /// 直接表现为断言失败而不是被静默吞掉。
    fn http_status(error: &BaseError) -> u16 {
        match error {
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

    /// 该组当前是否持有某条权限（按 `permission_group_item` 的事实行判断）。
    ///
    /// 自提权用例必须同时观测「成员关系」与「组条目」两个事实：只有二者同时成立，
    /// 调用者的有效权限才真的变大了——只看其中一个会把「权限已写入但人还没进组」
    /// 这类无害中间态误报成提权。
    pub async fn group_holds_item(app: &BuiltApp, group_id: i64, permission: &str) -> bool {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM permission_group_item WHERE group_id = ? AND permission = ?",
        )
        .bind(group_id)
        .bind(permission)
        .fetch_one(pool_of(app))
        .await
        .unwrap_or_else(|error| panic!("查询组 {group_id} 的条目 {permission} 失败: {error}"));
        count > 0
    }

    /// 读取权限目录（`GET /api/v1/access/permissions`），返回每条条目的原始 JSON。
    ///
    /// 直接读 JSON 而不是复用应用侧结构体：集成测试是独立 crate，`PermissionEntry`
    /// 是 `pub(crate)`，而本用例要钉住的正是**传输层真的把危害面标记发出去了**。
    pub async fn list_permission_entries(app: &BuiltApp, operator: &Admin) -> Vec<Value> {
        let authorization = format!("Bearer {}", operator.token);
        let response = dispatch(
            app,
            "access.grants",
            "list_permissions",
            json!({}),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
        .unwrap_or_else(|error| panic!("读取权限目录失败: {error}"));
        response
            .data
            .as_ref()
            .and_then(|data| data["permissions"].as_array())
            .cloned()
            .unwrap_or_else(|| panic!("权限目录响应缺少 permissions 数组: {response:?}"))
    }

    /// 目录里某条已声明权限的 `admin_equivalent` 标记；权限未声明时 panic。
    ///
    /// 返回 `Option<bool>` 而不是 `bool`：标记字段**整体缺失**（尚未实现）与
    /// 「标记为 false」是两件事，前者必须让用例红，不能被 `unwrap_or(false)` 吞掉。
    pub async fn catalog_flag_of(
        app: &BuiltApp,
        operator: &Admin,
        permission: &str,
    ) -> Option<bool> {
        list_permission_entries(app, operator)
            .await
            .iter()
            .find(|entry| entry["permission"].as_str() == Some(permission))
            .unwrap_or_else(|| panic!("目录中必须声明 {permission}"))["admin_equivalent"]
            .as_bool()
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

    /// 直写一条组条目行并**原样返回**数据库结果（用于断言约束兜底，不 panic）。
    ///
    /// 与 [`insert_item_row`] 同列口径，区别只在于把错误交回调用方：本函数专门用来
    /// 断言 `permission_group_item.group_id → permission_group.id` 这条外键真的存在
    /// （MySQL 1452），以及「同一条 INSERT 在目标组存在时成功」这条反向对照。
    pub async fn try_insert_item_row(
        app: &BuiltApp,
        group_id: i64,
        permission: &str,
        granted_by: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO permission_group_item (group_id, permission, granted_by, occurred_at) \
             VALUES (?, ?, ?, UNIX_TIMESTAMP())",
        )
        .bind(group_id)
        .bind(permission)
        .bind(granted_by)
        .execute(pool_of(app))
        .await
        .map(|_| ())
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

    /// 该 Access Token 是否已被判定为**不可用**，接受「已撤销」与「版本过期」两种死法。
    ///
    /// [`member_token_is_stale`] 只认 `AuthorizationStale`，而撤销路径的
    /// `revoke_by_subject` 会把主题的 Token **直接标为已撤销**（`TokenRevoked`）——
    /// 那是比版本过期更强的即时收敛。撤销用例的探针必须同时接受这两种形态。
    pub async fn member_token_is_dead(app: &BuiltApp, token: &str) -> bool {
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
            Err(BaseError::TokenRevoked) | Err(BaseError::AuthorizationStale) => true,
            Err(other) => panic!("Token 可用性探针返回预期外错误: {other}"),
        }
    }

    /// 组条目写 Action 的请求体构造。
    fn item_body(group_id: i64, permission: &str) -> Value {
        json!({ "group_id": group_id, "permission": permission })
    }

    /// 驱动一个组条目写 Action，返回原始结果。
    async fn item_mutation(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        permission: &str,
    ) -> Result<ApiResponse, BaseError> {
        authed_dispatch(
            app,
            "access.groups",
            action,
            item_body(group_id, permission),
            operator,
        )
        .await
    }

    /// 驱动一个组条目写 Action，折算为（状态码, 消息）。
    ///
    /// 并发用例直接经这条路径投出请求，Barrier 对齐请求起点，事务临界段的重叠
    /// 由真库的独立连接与行锁保证。
    pub async fn item_mutation_outcome(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        permission: &str,
    ) -> (u16, String) {
        match item_mutation(app, operator, action, group_id, permission).await {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 向组追加一条权限；失败即 panic（调用方断言的是成功路径）。
    pub async fn add_group_item(app: &BuiltApp, operator: &Admin, group_id: i64, permission: &str) {
        item_mutation(app, operator, "add_group_item", group_id, permission)
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
        match item_mutation(app, operator, "add_group_item", group_id, permission).await {
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
        match item_mutation(app, operator, "remove_group_item", group_id, permission).await {
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

    // -----------------------------------------------------------------------
    // Task 13 起：账号生命周期夹具（最后管理员守卫与账号删除的授权事实清理）。
    // -----------------------------------------------------------------------

    /// 尝试管理端停用目标账号并折算为 HTTP 状态码。
    ///
    /// `admin_disable_user` 要求 `account.users.manage`，因此操作者
    /// 身份由调用方给出——最后管理员用例刻意让一个**非系统管理员**来当操作者，
    /// 否则该 Action 既有的「不能停用自己」前置会先把用例挡在守卫之前。
    ///
    /// 目标用户是**路径参数**（`/api/v1/users/{id}/disable`），必须经
    /// `authed_dispatch_with_path` 传入 `request.path_params`。
    pub async fn admin_disable_status(
        app: &BuiltApp,
        operator: &Admin,
        target_user_id: i64,
    ) -> u16 {
        let target = target_user_id.to_string();
        match authed_dispatch_with_path(
            app,
            "account.user",
            "admin_disable_user",
            json!({}),
            operator,
            &[("id", target.as_str())],
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试管理端启用目标账号并折算为 HTTP 状态码。
    ///
    /// 与 [`admin_disable_status`] 完全对称：`admin_enable_user` 同样要求
    /// `account.users.manage`，目标用户也是**路径参数**
    /// （`/api/v1/users/{id}/enable`），必须经 `authed_dispatch_with_path` 传入
    /// `request.path_params`。
    pub async fn admin_enable_status(app: &BuiltApp, operator: &Admin, target_user_id: i64) -> u16 {
        let target = target_user_id.to_string();
        match authed_dispatch_with_path(
            app,
            "account.user",
            "admin_enable_user",
            json!({}),
            operator,
            &[("id", target.as_str())],
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试管理端签发密码重置凭证并折算为 HTTP 状态码。
    ///
    /// `admin_issue_password_reset` 的 `input.id` 是**路径参数**
    /// （`/api/v1/users/{id}/password-reset-tokens`）。这里只观测「调用是否被接受」，
    /// 刻意不读响应里的明文凭证——明文只回显一次，测试不需要它。
    pub async fn admin_issue_password_reset_status(
        app: &BuiltApp,
        operator: &Admin,
        target_user_id: i64,
    ) -> u16 {
        let target = target_user_id.to_string();
        match authed_dispatch_with_path(
            app,
            "account.user",
            "admin_issue_password_reset",
            json!({}),
            operator,
            &[("id", target.as_str())],
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试自助停用当前账号并折算为 HTTP 状态码。
    pub async fn disable_self_status(app: &BuiltApp, actor: &Admin) -> u16 {
        match authed_dispatch(app, "account.user", "disable_self", json!({}), actor).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试匿名化删除当前账号并折算为 HTTP 状态码。
    pub async fn delete_account_status(app: &BuiltApp, actor: &Admin) -> u16 {
        match authed_dispatch(
            app,
            "account.user",
            "delete_account",
            json!({ "confirmation": "delete my account" }),
            actor,
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 走真实删除路径删掉自己的账号；失败即 panic（调用方断言的是成功路径）。
    pub async fn delete_account(app: &BuiltApp, actor: &Admin) {
        let status = delete_account_status(app, actor).await;
        assert_eq!(status, 200, "删除账号必须成功");
    }

    /// 一个组里当前处于**启用**状态（`users.status = 'active'`）的成员数。
    ///
    /// spec §8.2 的不变量是「系统始终至少有一名启用管理员」，所以观测必须落在
    /// 「成员行 ∩ 启用账号」上：只看成员行会把已停用的成员算进去，从而读不出
    /// 「管理员被清零」这个真正的违规。
    pub async fn active_member_count(app: &BuiltApp, group_id: i64) -> u64 {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_group ug JOIN users u ON u.id = ug.user_id \
             WHERE ug.group_id = ? AND u.status = 'active'",
        )
        .bind(group_id)
        .fetch_one(pool_of(app))
        .await
        .unwrap_or_else(|error| panic!("统计组 {group_id} 的启用成员失败: {error}"));
        u64::try_from(count).unwrap_or_else(|error| panic!("启用成员数为负: {error}"))
    }

    /// 账号当前的 `users.status` 列取值。
    pub async fn user_status(app: &BuiltApp, user_id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(pool_of(app))
            .await
            .unwrap_or_else(|error| panic!("读取用户 {user_id} 状态失败: {error}"))
    }

    /// 某个用户名下残留的 `authz_grant` 直授行数。
    pub async fn grant_rows_of_user(app: &BuiltApp, user_id: i64) -> u64 {
        count_rows_of_user(app, "authz_grant", user_id).await
    }

    /// 某个用户名下残留的 `user_group` 成员行数。
    pub async fn member_rows_of_user(app: &BuiltApp, user_id: i64) -> u64 {
        count_rows_of_user(app, "user_group", user_id).await
    }

    /// 按 `user_id` 统计一张授权事实表的行数。
    async fn count_rows_of_user(app: &BuiltApp, table: &str, user_id: i64) -> u64 {
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE user_id = ?"))
                .bind(user_id)
                .fetch_one(pool_of(app))
                .await
                .unwrap_or_else(|error| {
                    panic!("统计 {table} 中用户 {user_id} 的行数失败: {error}")
                });
        u64::try_from(count).unwrap_or_else(|error| panic!("{table} 行数为负: {error}"))
    }

    /// 按 `group_key` 统计组行数（重复 key 用例：被拒的创建不得留下第二行）。
    pub async fn group_rows_of_key(app: &BuiltApp, group_key: &str) -> u64 {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM permission_group WHERE group_key = ?")
                .bind(group_key)
                .fetch_one(pool_of(app))
                .await
                .unwrap_or_else(|error| panic!("统计权限组 {group_key} 行数失败: {error}"));
        u64::try_from(count).unwrap_or_else(|error| panic!("组行数为负: {error}"))
    }

    /// 尝试改组展示信息并折算为（HTTP 状态码, 消息）。
    ///
    /// 成功时消息是成功文案（用例只会在拒绝路径上读它）。
    pub async fn update_group_outcome(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
        title: &str,
    ) -> (u16, String) {
        match authed_dispatch(
            app,
            "access.groups",
            "update_group",
            json!({ "group_id": group_id, "title": title }),
            operator,
        )
        .await
        {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 撤销直授权限，折算为（HTTP 状态码, 消息）。
    ///
    /// 与 [`grant_permission_status`] 同构，只是目标 Action 换成 `revoke_permission`——
    /// 撤销路径同样要求 `access.grants.write`。
    pub async fn revoke_permission_outcome(
        app: &BuiltApp,
        operator: &Admin,
        user_id: i64,
        permission: &str,
    ) -> (u16, String) {
        match authed_dispatch(
            app,
            "access.grants",
            "revoke_permission",
            json!({ "user_id": user_id, "permission": permission }),
            operator,
        )
        .await
        {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 查询目标用户的直授权限（`GET /api/v1/access/users/{user_id}/grants`），返回原始结果。
    ///
    /// `user_id` 是路径参数，必须走 `path_params`（写进 body
    /// 会被判为缺参，见 [`dispatch_with_path`] 的说明）。
    pub async fn list_user_grants(
        app: &BuiltApp,
        operator: &Admin,
        user_id: i64,
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", operator.token);
        let user_id_param = user_id.to_string();
        dispatch_with_path(
            app,
            "access.grants",
            "list_user_grants",
            json!({}),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
            &[("user_id", user_id_param.as_str())],
        )
        .await
    }

    /// 尝试查询目标用户直授权限并折算为（HTTP 状态码, 消息）。
    pub async fn list_user_grants_outcome(
        app: &BuiltApp,
        operator: &Admin,
        user_id: i64,
    ) -> (u16, String) {
        match list_user_grants(app, operator, user_id).await {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // 直授批量/过期任务起：批量授予/撤销夹具。
    // -----------------------------------------------------------------------

    /// 批量条目构造：无过期时间（缺省 = 永久有效）。
    pub fn grant_item(user_id: i64, permission: &str) -> Value {
        json!({ "user_id": user_id, "permission": permission })
    }

    /// 批量条目构造：带过期时间（Unix 秒）。
    pub fn grant_item_with_expiry(user_id: i64, permission: &str, expires_at: i64) -> Value {
        json!({ "user_id": user_id, "permission": permission, "expires_at": expires_at })
    }

    /// 批量撤销条目构造。
    pub fn revoke_item(user_id: i64, permission: &str) -> Value {
        json!({ "user_id": user_id, "permission": permission })
    }

    /// 驱动批量授予 Action，返回原始结果。
    pub async fn batch_grant_permissions(
        app: &BuiltApp,
        operator: &Admin,
        items: &[Value],
    ) -> Result<ApiResponse, BaseError> {
        authed_dispatch(
            app,
            "access.grants",
            "batch_grant_permissions",
            json!({ "items": items }),
            operator,
        )
        .await
    }

    /// 尝试批量授予并折算为（HTTP 状态码, 消息）：拒绝路径（G2 闸门 / 超限）观测被拒。
    pub async fn batch_grant_permissions_outcome(
        app: &BuiltApp,
        operator: &Admin,
        items: &[Value],
    ) -> (u16, String) {
        match batch_grant_permissions(app, operator, items).await {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 驱动批量撤销 Action，返回原始结果。
    pub async fn batch_revoke_permissions(
        app: &BuiltApp,
        operator: &Admin,
        items: &[Value],
    ) -> Result<ApiResponse, BaseError> {
        authed_dispatch(
            app,
            "access.grants",
            "batch_revoke_permissions",
            json!({ "items": items }),
            operator,
        )
        .await
    }

    /// 尝试批量撤销并折算为（HTTP 状态码, 消息）：拒绝路径观测被拒。
    pub async fn batch_revoke_permissions_outcome(
        app: &BuiltApp,
        operator: &Admin,
        items: &[Value],
    ) -> (u16, String) {
        match batch_revoke_permissions(app, operator, items).await {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 列出当前操作者可见的全部组 key（`list_groups` 响应的 group_key 集合，升序）。
    pub async fn list_group_keys(app: &BuiltApp, operator: &Admin) -> Vec<String> {
        let authorization = format!("Bearer {}", operator.token);
        let response = dispatch(
            app,
            "access.groups",
            "list_groups",
            json!({}),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
        .unwrap_or_else(|error| panic!("列表权限组失败: {error}"));
        assert_eq!(response.code, 0, "列表权限组必须成功: {}", response.message);
        let mut keys: Vec<String> = response
            .data
            .as_ref()
            .unwrap_or_else(|| panic!("列表响应缺少 data: {response:?}"))
            .get("groups")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("groups 必须是数组: {response:?}"))
            .iter()
            .map(|group| {
                group["group_key"]
                    .as_str()
                    .unwrap_or_else(|| panic!("组条目缺少 group_key: {group}"))
                    .to_string()
            })
            .collect();
        keys.sort();
        keys
    }

    /// 查询组详情并折算为（HTTP 状态码, 消息）。
    ///
    /// 可见性用例需要断言「不可见的组与不存在的组统一 404（RecordNotFound 同形态）」，
    /// 因此必须把状态码与消息都拿回来做同形态比较。
    pub async fn get_group_outcome(
        app: &BuiltApp,
        operator: &Admin,
        group_id: i64,
    ) -> (u16, String) {
        let authorization = format!("Bearer {}", operator.token);
        let group_id_param = group_id.to_string();
        match dispatch_with_path(
            app,
            "access.groups",
            "get_group",
            json!({}),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
            &[("group_id", group_id_param.as_str())],
        )
        .await
        {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 从 `list_user_grants` 响应中取目标权限的视图条目（找不到返回 None）。
    ///
    /// 用例需要按权限名定位单条视图（`expires_at` / `expired` 字段），不关心其余条目。
    pub async fn grant_view_of(
        app: &BuiltApp,
        operator: &Admin,
        user_id: i64,
        permission: &str,
    ) -> Option<Value> {
        let response = list_user_grants(app, operator, user_id)
            .await
            .unwrap_or_else(|error| panic!("查询用户授权失败: {error}"));
        response
            .data
            .as_ref()
            .unwrap_or_else(|| panic!("查询响应缺少 data: {response:?}"))
            .get("grants")
            .and_then(Value::as_array)
            .and_then(|grants| {
                grants
                    .iter()
                    .find(|grant| grant["permission"].as_str() == Some(permission))
                    .cloned()
            })
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

// ---------------------------------------------------------------------------
// Task 13 起的账号生命周期用例：最后管理员守卫与账号删除的孤儿授权清理。
// ---------------------------------------------------------------------------

/// spec §8.2：最后一名系统管理员不可被停用、不可自停用、不可被删除。
///
/// 计划此处断言 409（设计 §9.3 的 `Conflict`）。`yang_base::BaseError` 没有该变体，
/// 且设计同节要求「不为个别用例扩展框架错误类型」，因此拒绝落成既有的 `ParamInvalid`
/// （400）——与 `remove_group_member` 的最后管理员守卫同一取舍。断言要钉住的事实不变：
/// 三条路径都必须被拒、绝不能是 5xx，且拒绝后账号状态与成员关系原封不动。
///
/// **停用路径落到 403 而非 400**：操作者刻意选非全权组成员，于是先撞上 §8.1 的
/// admin-only 守卫（`a_non_admin_cannot_disable_a_system_admin_member` 钉的就是它）。
/// 两条守卫在停用路径上的相对次序决定了这一点：admin-only 判定在前，最后管理员判定在后，
/// 而非管理员根本没有资格停用全权组成员，也就走不到 §8.2 那一步。这不削弱本条要钉的不变量
/// （最后一名管理员一样没被停用，状态与成员行原封不动），只是说明在 `admin_disable_user`
/// 上 §8.2 已被 §8.1 蕴含：任何已认证的操作者只要本身是全权组成员，就自己也是启用管理员，
/// 目标便不可能是「最后一名」。删除与自停用两条路径没有这条前置，仍由 §8.2 以 400 拒绝。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn the_last_system_admin_cannot_be_disabled_or_deleted() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 停用他人需要 `account.users.manage`；这个操作者**不是**系统管理员，
    // 否则 `admin_disable_user` 既有的「不能停用自己」前置会先把用例挡在守卫之前，
    // 断言就变成了在同义反复地复述那条前置。
    let manager = harness::grant_only(&app, &admin, "manager", "account.users.manage").await;

    let disable_status = harness::admin_disable_status(&app, &manager, admin.user_id).await;
    assert_eq!(
        disable_status, 403,
        "非全权组成员不得停用全权组成员（§8.1 admin-only 守卫先于 §8.2 生效）"
    );

    let delete_status = harness::delete_account_status(&app, &admin).await;
    assert_eq!(delete_status, 400, "最后一名系统管理员不可被删除");

    let self_disable_status = harness::disable_self_status(&app, &admin).await;
    assert_eq!(self_disable_status, 400, "最后一名系统管理员不可自停用");

    // 拒绝不得留下任何写结果。
    assert_eq!(
        harness::user_status(&app, admin.user_id).await,
        "active",
        "被拒的停用不得改动账号状态"
    );
    assert!(
        harness::is_member(
            &app,
            harness::system_admin_group_id(&app).await,
            admin.user_id
        )
        .await,
        "被拒的操作不得顺手删掉全权组成员行"
    );
}

/// 有两名管理员时，停用其一必须成功（守卫不能过度收紧）。
///
/// 它是 [`a_non_admin_cannot_disable_a_system_admin_member`] 的活性对照：操作者
/// `first` **就是**全权组成员，因此 §8.1 的 admin-only 守卫必须放行——若守卫被写成
/// 「一律拒绝」，只有这一条会变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn one_of_two_admins_can_be_disabled() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;

    let status = harness::admin_disable_status(&app, &first, second.user_id).await;
    assert_eq!(status, 200, "还有另一名启用管理员时必须允许停用");
    assert_eq!(
        harness::user_status(&app, second.user_id).await,
        "disabled",
        "停用必须真的落库"
    );
}

/// spec §8.1 附加规则：**只有全权组成员能修改全权组成员**——管理端停用路径同样适用。
///
/// 缺陷形态：`add_group_member` / `remove_group_member` 都有这条 admin-only 守卫，
/// `admin_disable_user` 只有 §8.2 的最后管理员判定。于是持 `account.users.manage` 的
/// 非管理员可以反复停用全权组成员——只要每次停用后组内仍剩 >=1 名启用管理员，最后管理员
/// 判定就一路放行，直到只剩他指定的那一名。停用与移出的区别只在于目标账号多了一层
/// 「被停用」，因此这条守卫必须与成员变更路径完全对称。
///
/// 守卫不得过度收紧的对照由 [`one_of_two_admins_can_be_disabled`] 覆盖：全权组成员停用
/// 另一名全权组成员必须仍然放行（200）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_disable_a_system_admin_member() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;
    // 攻击者：只直授 `account.users.manage`，完全不在全权组内。
    let outsider = harness::grant_only(&app, &first, "outsider", "account.users.manage").await;
    let group_id = harness::system_admin_group_id(&app).await;

    // 夹具必须从「两名启用管理员」出发：此时最后管理员判定会放行（去掉一个仍剩一个），
    // 因此这一次停用能否被拦下，只取决于 admin-only 守卫在不在。
    assert_eq!(
        harness::active_member_count(&app, group_id).await,
        2,
        "夹具必须从两名启用管理员出发，否则最后管理员判定会先拦下，用例测不到 admin-only 守卫"
    );

    let status = harness::admin_disable_status(&app, &outsider, second.user_id).await;
    assert_eq!(
        status, 403,
        "非全权组成员不得修改全权组成员，必须 403 PermissionDenied"
    );
    assert_eq!(
        harness::user_status(&app, second.user_id).await,
        "active",
        "被拒的停用不得改动账号状态"
    );
    assert!(
        harness::is_member(&app, group_id, second.user_id).await,
        "被拒的操作不得顺手删掉全权组成员行"
    );
}

/// spec §8.1 附加规则：管理端**启用**路径必须与停用/成员变更完全对称。
///
/// 缺陷形态：`admin_enable_user` 只声明 `account.users.manage`，没有任何 admin-only
/// 守卫。非管理员于是能重新启用一名被合法停用的全权组成员，撤销合法停用决定、让被停用者
/// 的权限与会话复活——与 `admin_disable_user` 缺少守卫时是同一类越权，只是方向相反。
/// 目标先由另一名管理员合法停用，因此启用动作能走到守卫，而不是先被状态前置拦下。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_enable_a_disabled_system_admin_member() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;
    // 攻击者：只直授 `account.users.manage`，完全不在全权组内。
    let outsider = harness::grant_only(&app, &first, "outsider", "account.users.manage").await;

    // 前置：由全权组成员合法停用 second（还有 first 在，§8.2 最后管理员判定放行）。
    let disable_status = harness::admin_disable_status(&app, &first, second.user_id).await;
    assert_eq!(
        disable_status, 200,
        "夹具前置：管理员停用第二管理员必须成功"
    );
    assert_eq!(
        harness::user_status(&app, second.user_id).await,
        "disabled",
        "夹具前置：second 必须真的处于停用"
    );

    let status = harness::admin_enable_status(&app, &outsider, second.user_id).await;
    assert_eq!(
        status, 403,
        "非全权组成员不得修改全权组成员，必须 403 PermissionDenied"
    );
    assert_eq!(
        harness::user_status(&app, second.user_id).await,
        "disabled",
        "被拒的启用不得把账号翻回 active"
    );
}

/// [`a_non_admin_cannot_enable_a_disabled_system_admin_member`] 的活性对照：
/// 全权组成员启用另一名被停用的全权组成员必须仍然放行（守卫不得过度收紧）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_system_admin_can_enable_a_disabled_system_admin_member() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;

    let disable_status = harness::admin_disable_status(&app, &first, second.user_id).await;
    assert_eq!(
        disable_status, 200,
        "夹具前置：管理员停用第二管理员必须成功"
    );

    let status = harness::admin_enable_status(&app, &first, second.user_id).await;
    assert_eq!(status, 200, "全权组成员启用被停用的全权组成员必须允许");
    assert_eq!(
        harness::user_status(&app, second.user_id).await,
        "active",
        "启用必须真的落库"
    );
}

/// 凭据签发必须与日常用户管理分属两个权限：只持 `account.users.manage` 的调用者
/// 不得签发密码重置凭证。
///
/// 缺陷形态：`admin_issue_password_reset` 与停用/启用共用 `account.users.manage`，
/// 且目标取自路径参数、对任意账号签发。持该权限的非管理员于是可以给系统管理员签发
/// 重置凭证、重置其口令并登录成他，一步拿到全部权限——`account.users.manage` 实质
/// 等价于 root。拆分后该 Action 要求独立权限 `account.users.reset_credentials`，
/// 只持 manage 的调用者必须被拒。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn only_account_users_manage_cannot_issue_password_reset() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 攻击者：只持 `account.users.manage`，即拆分前的「管理用户」权限。
    let manager = harness::grant_only(&app, &admin, "manager", "account.users.manage").await;

    let status = harness::admin_issue_password_reset_status(&app, &manager, admin.user_id).await;
    assert_eq!(
        status, 403,
        "只持 account.users.manage 不得签发重置凭证（已不再持有该权限）"
    );
}

/// [`only_account_users_manage_cannot_issue_password_reset`] 的活性对照：持新权限
/// `account.users.reset_credentials` 的调用者必须能正常签发（拆分不得把功能一并锁死）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn the_split_reset_credential_permission_can_issue_password_reset() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let issuer =
        harness::grant_only(&app, &admin, "issuer", "account.users.reset_credentials").await;

    let status = harness::admin_issue_password_reset_status(&app, &issuer, admin.user_id).await;
    assert_eq!(
        status, 200,
        "持有 account.users.reset_credentials 必须能签发"
    );
}

/// 拆分是**声明层**的事实：`admin_issue_password_reset` 只能声明
/// `account.users.reset_credentials`，不得再挂着 `account.users.manage`。
///
/// 只断言运行期行为不够——若有人把 `account.users.manage` 加回来（或两权限并列），
/// 上面两条行为用例仍可能通过，本用例才会变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn admin_issue_password_reset_declares_only_the_split_permission() {
    let app = harness::build_test_app().await;
    let mut permissions: Vec<String> = Vec::new();
    for addon in app.catalog().addons() {
        for module in &addon.modules {
            for action in module.actions() {
                if module.name.as_str() == "account.user"
                    && action.name.as_str() == "admin_issue_password_reset"
                {
                    permissions = action.permissions.clone();
                }
            }
        }
    }
    assert_eq!(
        permissions,
        vec!["account.users.reset_credentials".to_string()],
        "签发重置凭证必须只声明拆分后的独立权限"
    );
}

/// spec §8.3：账号删除后不得残留授权事实。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn deleting_an_account_leaves_no_orphan_authorization_rows() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let user = harness::grant_only(&app, &admin, "victim", "feishu.datasource.read").await;
    let group_id = harness::create_group(&app, &admin, "temp", "临时").await;
    harness::add_member(&app, &admin, group_id, user.user_id).await;

    // 前置事实必须真的存在，否则「删除后为 0」可能只是从未建起来过。
    assert_eq!(
        harness::grant_rows_of_user(&app, user.user_id).await,
        1,
        "夹具必须真的写入一条直授权限"
    );
    assert_eq!(
        harness::member_rows_of_user(&app, user.user_id).await,
        1,
        "夹具必须真的写入一条成员关系"
    );

    // 入组递增了 victim 的授权版本，删除前必须换发令牌，否则用例会停在 401。
    let user = harness::relogin(&app, &user).await;
    harness::delete_account(&app, &user).await;

    assert_eq!(
        harness::grant_rows_of_user(&app, user.user_id).await,
        0,
        "不得残留 authz_grant 行"
    );
    assert_eq!(
        harness::member_rows_of_user(&app, user.user_id).await,
        0,
        "不得残留 user_group 行"
    );
}

// ---------------------------------------------------------------------------
// 最后管理员守卫的原子性（spec §8.2 的不可达态）。
// ---------------------------------------------------------------------------

/// spec §8.2：**无论并发如何交错**，系统都不得失去最后一名启用管理员。
///
/// 两个管理员各自自助停用，用真库两条独立连接 + Barrier 让两次请求被同时投出。
/// （Barrier 对齐的是请求起点：`wait()` 之后还有鉴权与授权检查，才进事务；
/// 两次「读管理员计数 → 写停用」的重叠由独立连接与 `users` 行锁保证，不是 Barrier 的功劳。）
/// 若计数是在事务外经连接池无锁读出的（缺陷形态），两边都会
/// 读到「还有两名」，双双放行，system_admin 组被清零；只有把成员行在**调用方事务内**
/// 按 `user_id` 升序 `FOR UPDATE` 逐个锁定后再判启用状态，第二个进入者才会看到
/// 第一个的提交从而被守卫拦下。
///
/// 断言只钉住不安全的那件事——最终必须仍有至少一名启用管理员；这正是缺陷会打破、
/// 而修复必须守住的不变量。至于败者是被守卫拦下（400）还是被 MySQL 挑中回滚
/// （500，两次事务互相持有对方随后还要锁的成员行时 InnoDB 会牺牲一个），在真库上
/// 两种都会实测出现，且都不影响该不变量：被回滚的一方原子地什么都没写。因此这里
/// **不**断言「绝不出现 5xx」——那会要求放弃行锁，退回到本项要消灭的缺陷形态。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_self_disables_never_leave_zero_active_admins() {
    // 单轮的并发交错仍可能被调度偶然化：两个任务恰好一前一后抵达，缺陷里那次
    // 事务外无锁读就可能读到对方的已提交结果而侥幸放行。所以重复三轮，每轮都由
    // `build_test_app` 重建库、重新搭出「恰好两名启用管理员」——只要有一轮被清零，
    // 就说明守卫并非原子。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let first = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &first, "admin2").await;
        let group_id = harness::system_admin_group_id(&app).await;
        assert_eq!(
            harness::active_member_count(&app, group_id).await,
            2,
            "第 {round} 轮夹具必须从两名启用管理员出发，否则并发窗口压不出来"
        );

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for actor in [first.clone(), second.clone()] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::disable_self_status(&app, &actor).await
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|e| panic!("任务不应 panic: {e}")),
            );
        }

        let active = harness::active_member_count(&app, group_id).await;
        assert!(
            active >= 1,
            "第 {round} 轮并发自助停用两名管理员后系统零管理员（响应 {statuses:?}）\
             ——spec §8.2 的不可达态被触达"
        );
        // 恰好一条成功：这是「两个请求真的撞上了」的活性证据，两个方向都不能松。
        //   - 两条都 200 ⇒ 守卫不原子，组被清零（已被上一条断言钉住）；
        //   - 两条都不是 200（例如 [500,500] 双双被回滚、[400,400] 全部提前出局）⇒
        //     什么都没发生，用例退化成同义反复，同样是回归。
        // 设计上必然恰好一条 200：先到者通过守卫并停用自己；后到者或被守卫拒绝（400，
        // 它读到先到者已停用、再停用自己就零管理员），或被 MySQL 挑中做死锁牺牲者并
        // 原子回滚（500）——被回滚的一方什么都没写。这里因此不再写 `<= 1`：那个较宽的
        // 形式会被 [500,500] / [400,400] 满足，恰好漏掉上面第二种回归。
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮两个并发停用必须恰好一条成功（既不得双双放行，也不得双双被拒），\
             实际响应 {statuses:?}"
        );
    }
}

/// spec §8.2 的守卫语义是「本次操作之后仍有至少一名启用管理员」，
/// 而不是「当前至少有两名」。
///
/// 移出一名**已停用**的管理员成员不会减少启用管理员数：即便组里此刻只剩一名
/// 启用管理员，这次移出也完全合法。裸计数（`admins <= 1` 就拒）会把这种合法操作
/// 误拒，因此这一条钉住的是「计数必须能表达目标自身是否已被计入」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn removing_an_already_disabled_admin_member_is_not_rejected() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;
    let group_id = harness::system_admin_group_id(&app).await;

    // 两名启用管理员时停用其一：守卫必须放行（否则下面的夹具搭不起来）。
    assert_eq!(
        harness::admin_disable_status(&app, &first, second.user_id).await,
        200,
        "两名启用管理员时停用其一必须成功"
    );
    assert_eq!(
        harness::user_status(&app, second.user_id).await,
        "disabled",
        "停用必须真的落库"
    );
    assert_eq!(
        harness::active_member_count(&app, group_id).await,
        1,
        "夹具必须真的只剩一名启用管理员"
    );

    // 现在被移出的目标是一名**已停用**的管理员成员：这次移出不会让启用管理员
    // 变少（1 → 1），因此必须放行。
    let status = harness::remove_member_status(&app, &first, group_id, second.user_id).await;
    assert_eq!(
        status, 200,
        "移出一名已停用的管理员成员不减少启用管理员数，必须放行（不得 400/500）"
    );
    assert!(
        !harness::is_member(&app, group_id, second.user_id).await,
        "放行必须真的删掉成员行"
    );
    assert_eq!(
        harness::active_member_count(&app, group_id).await,
        1,
        "移出已停用成员后启用管理员数不变"
    );
}

// ---------------------------------------------------------------------------
// 组成员接口的两条守卫：全权组 admin-only 与成员上限（含并发幂等）。
// ---------------------------------------------------------------------------

/// spec §8.1 附加规则：**只有全权组成员能修改全权组成员**——移除路径同样适用。
///
/// 缺陷形态：`remove_group_member` 只有 §8.2 的最后管理员判定，没有 `add_group_member`
/// 那条 admin-only 守卫。于是持 `access.groups.write` 的非管理员可以把管理员一个个
/// 移出——只要组内始终还剩 >=2 名启用管理员，最后管理员判定就一直放行，攻击者可以
/// 反复执行直到组内只剩他指定的那一名。这条守卫必须独立于最后管理员判定，且先于它。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_remove_members_of_the_system_admin_group() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    let second = harness::add_second_admin(&app, &first, "admin2").await;
    // 攻击者：只直授 `access.groups.write`，完全不在全权组内。
    let outsider = harness::grant_only(&app, &first, "outsider", "access.groups.write").await;
    let group_id = harness::system_admin_group_id(&app).await;

    // 夹具必须真的从「两名启用管理员」出发：此时最后管理员判定会放行（去掉一个仍剩一个），
    // 因此这一次移除能否被拦下，只取决于 admin-only 守卫在不在。
    assert_eq!(
        harness::active_member_count(&app, group_id).await,
        2,
        "夹具必须从两名启用管理员出发，否则最后管理员判定会先拦下，用例测不到 admin-only 守卫"
    );

    let status = harness::remove_member_status(&app, &outsider, group_id, second.user_id).await;
    assert_eq!(
        status, 403,
        "非全权组成员不得修改全权组成员，必须 403 PermissionDenied"
    );
    assert!(
        harness::is_member(&app, group_id, second.user_id).await,
        "被拒的移除不得删掉成员行"
    );
}

/// spec 的成员上限（200）：`add_group_member` 是唯一能增加成员的路径，必须自己守住上限。
///
/// 缺陷形态：全仓 `ensure_member_limit` 只出现在 `add_group_item` / `remove_group_item`，
/// `add_group_member` 从不调用它，于是组可经 API 无界增长，`repository.rs` 里
/// 「成员集合被 `MAX_GROUP_MEMBERS` 约束成有界规模」的注释与事实不符。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn adding_a_member_beyond_the_limit_is_rejected() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "capped", "受上限约束的组").await;

    // 极限测试要 200 名成员，走真实注册路径要跑 200 次 argon2，因此与既有夹具同例直写。
    let seeded = harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS).await;
    assert_eq!(
        seeded,
        harness::MAX_GROUP_MEMBERS,
        "夹具必须先把组填到恰好上限"
    );

    let newcomer = harness::register_with_code(&app, "latecomer", "latecomer@example.com")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let (status, message) = harness::add_member_outcome(&app, &admin, group_id, newcomer).await;

    assert_eq!(
        status,
        400,
        "第 {} 名成员必须被明确拒绝（ParamInvalid→400），绝不能是 5xx：{message}",
        harness::MAX_GROUP_MEMBERS + 1
    );
    assert!(
        message.contains("上限"),
        "拒绝必须说明撞上的是成员上限，而不是别的参数错误，实际 {message}"
    );
    assert!(
        message.contains(&harness::MAX_GROUP_MEMBERS.to_string()),
        "错误信息必须给出上限 {}，实际 {message}",
        harness::MAX_GROUP_MEMBERS
    );
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
        "被拒的写入不得落库（组内仍应恰好 {} 人）",
        harness::MAX_GROUP_MEMBERS
    );
    assert!(
        !harness::is_member(&app, group_id, newcomer).await,
        "被拒的成员不得出现在组内"
    );
}

/// spec §9.2 的幂等契约在并发下同样成立：两个并发加的同一 (user, group)，终态恰好一行。
///
/// 缺陷形态：`insert_member_in_tx` 是「先查后插」，并发时后到者撞唯一键（1062）被
/// `DbError::ConstraintError` 捕获，而它同时覆盖外键（1452）——于是被折算成
/// `RecordNotFound("权限组")`（404），把「已是成员」这个幂等成功谎报成「组已消失」。
///
/// 用真库两条独立连接 + Barrier 让两次插入真正撞在唯一键上（真库上后到者会先阻塞在
/// 索引锁上，等先到者提交后拿到 1062），单连接串行调用压不出这个窗口。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_duplicate_adds_of_the_same_member_stay_idempotent() {
    // 单轮交错仍可能被调度偶然化：两个任务若恰好一前一后跑完，后到者会在「先查」里
    // 读到已提交的成员行而直接幂等返回，压不到唯一键冲突那条路径。重复三轮。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "dup", "并发重复加成员").await;
        let target = harness::register_with_code(&app, "dup_target", "dup_target@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        // 并发先做完：两个任务各持一份请求。Barrier 对齐请求起点，
        // 两次插入是否真的撞在唯一键上由真库的独立连接与索引锁决定。
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let app = app.clone();
            let admin = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                // 用 outcome 而非 status：失败时要能读出真正的错误文案，
                // 否则 5xx/404 只留下一串状态码，定位不到是哪条错误被折算出来的。
                harness::member_mutation_outcome(&app, &admin, "add_group_member", group_id, target)
                    .await
            }));
        }
        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        let statuses: Vec<u16> = outcomes.iter().map(|(status, _)| *status).collect();

        // 幂等语义下两次都该是 200（一次 changed=true，一次 changed=false）。这里把它
        // 断言成「两个都是 200」比「不得是 5xx/404」更强：它同时钉住了不得把重复加
        // 当成错误，也钉住了不得泄漏 500。
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮并发重复加同一成员必须两次都幂等成功，实际 {outcomes:?}"
        );
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            1,
            "第 {round} 轮并发重复加同一成员必须收敛到恰好一行成员"
        );
    }
}

// ---------------------------------------------------------------------------
// R1（收尾回合）：成员关系读锁定化——成员变更必须串行化，且判据必须在持锁之后读。
// 串行化点与锁序的权威描述见 src/addon/access/domain/groups/admin.rs 的模块注释；
// 收尾回合已把串行化点从「成员行」改为「目标组组行」，此处不复述，避免再次漂移。
// ---------------------------------------------------------------------------

/// 用 Barrier 同时驱动两条组成员写请求。
///
/// 两个任务的操作者**分别给出**：并发用例必须能用两个不同的账号发起请求，否则两次
/// 请求会因为锁批里共有的操作者行而被串行化，从而压不出「两个并发请求各自读到同一份
/// 陈旧快照」这个窗口。
///
/// Barrier 对齐的是**请求起点**——`wait()` 之后直接进入事务，
/// 事务提交之后还有审计 INSERT——不是「开事务 → 读判据 → 写事实」那一段。它只是保证
/// 两次请求被同时投出，临界段的重叠由独立连接与行锁负责。
async fn concurrent_member_mutations(
    app: &BuiltApp,
    group_id: i64,
    left: (harness::Admin, &'static str, i64),
    right: (harness::Admin, &'static str, i64),
) -> Vec<u16> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for (operator, action, user_id) in [left, right] {
        let app = app.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            harness::member_mutation_outcome(&app, &operator, action, group_id, user_id)
                .await
                .0
        }));
    }
    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(
            handle
                .await
                .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
        );
    }
    statuses
}

/// spec §8.2 的不可达态在**成员移除**路径上同样不可达：并发移出管理员不得把组清零。
///
/// 缺陷形态：守卫的成员名单来自 `list_members_in_tx` 的**普通一致性读**，而「移出一个
/// 成员」改的是 `user_group` 的成员行、不是 `users.status`——守卫持有的 users 行锁因此
/// 完全串行化不了成员变更。两个并发的移出各自读到同一份陈旧名单 [A,B]、各自数出 2 名
/// 启用管理员，双双放行，`system_admin` 组成员归零（spec §8.2 明称该状态不可达）。
///
/// 真库 + 两条独立连接 + Barrier：Barrier 只对齐两次请求的**起点**（它之后还有鉴权与
/// 事务），事务临界段的重叠由两条独立连接与行锁负责。两个场景各跑三轮，避免调度偶然。
///
/// 断言只钉住不安全的那件事——最终必须仍有至少一名启用管理员，且两个请求不得都成功。
/// 败者是被守卫拦下（400/403）还是被授权版本水位线拦下（401）都无关紧要：三者都只是
/// 拒绝，不改变不变量。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_admin_removals_never_leave_zero_active_admins() {
    // 场景一：同一名管理员并发发两条移出——一条移出自己，一条移出另一名管理员。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let first = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &first, "admin2").await;
        let group_id = harness::system_admin_group_id(&app).await;
        assert_eq!(
            harness::active_member_count(&app, group_id).await,
            2,
            "第 {round} 轮场景一必须从两名启用管理员出发，否则并发窗口压不出来"
        );

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (first.clone(), "remove_group_member", second.user_id),
            (first.clone(), "remove_group_member", first.user_id),
        )
        .await;

        let active = harness::active_member_count(&app, group_id).await;
        assert!(
            active >= 1,
            "第 {round} 轮场景一：同一管理员并发移出自己与另一名管理员后系统零管理员\
             （响应 {statuses:?}）——spec §8.2 的不可达态被触达"
        );
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮场景一：两条并发移出恰好只能有一个成功（另一个必须被守卫或被\
             授权版本水位线拦下），实际响应 {statuses:?}"
        );
    }

    // 场景二：两名管理员**互相**移出对方（各自的操作者身份不同，锁集仍然相交）。
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let first = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &first, "admin2").await;
        let group_id = harness::system_admin_group_id(&app).await;
        assert_eq!(
            harness::active_member_count(&app, group_id).await,
            2,
            "第 {round} 轮场景二必须从两名启用管理员出发"
        );

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (operator, target) in [
            (first.clone(), second.user_id),
            (second.clone(), first.user_id),
        ] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "remove_group_member",
                    group_id,
                    target,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }

        let active = harness::active_member_count(&app, group_id).await;
        assert!(
            active >= 1,
            "第 {round} 轮场景二：两名管理员并发互相移出后系统零管理员\
             （响应 {statuses:?}）——spec §8.2 的不可达态被触达"
        );
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮场景二：互相移出恰好只能有一个成功（另一个必须被守卫拦下），\
             实际响应 {statuses:?}"
        );
    }
}

/// 成员上限必须真的封死：并发把两名**不同**成员加进只剩一个空位的组，终态不得越过 200。
///
/// 缺陷形态：`add_group_member` 的上限判定建立在 `list_members_in_tx` 的**非锁定快照**上。
/// 两名**不同操作者**并发加**不同**成员时，两个事务的锁批（操作者 + 目标）互不相交，
/// 请求因此真的并发执行、各自判定 `len + 1 <= 200` 合法，成员数越过 `MAX_GROUP_MEMBERS`
/// （`repository.rs` 里「成员集合被上限约束成有界规模」的注释随之失真）。
///
/// 操作者必须取两个不同账号：同一操作者的两次请求会因为锁批里共有的操作者行而被串行化，
/// 那样压不出这个窗口（实测：同一操作者时本用例在缺陷代码上照样绿）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_adds_cannot_exceed_the_member_limit() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "nearly_full", "只剩一个空位").await;
        let seeded =
            harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS - 1).await;
        assert_eq!(
            seeded,
            harness::MAX_GROUP_MEMBERS - 1,
            "第 {round} 轮夹具必须先把组填到只剩一个空位"
        );

        let left = harness::register_with_code(&app, "limit_left", "limit_left@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let right = harness::register_with_code(&app, "limit_right", "limit_right@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let left_operator =
            harness::grant_only(&app, &admin, "limit_op_left", "access.groups.write").await;
        let right_operator =
            harness::grant_only(&app, &admin, "limit_op_right", "access.groups.write").await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (left_operator, "add_group_member", left),
            (right_operator, "add_group_member", right),
        )
        .await;

        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮只剩一个空位时两次并发加成员只能成功一个，实际响应 {statuses:?}"
        );
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
            "第 {round} 轮并发加成员后组内必须恰好 {} 人，不得越过上限（响应 {statuses:?}）",
            harness::MAX_GROUP_MEMBERS
        );
    }
}

/// 并发把两名**不同**成员加进同一个**空**组：两次都合法（两名成员，上限 200 之内），
/// 必须双双成功，绝不能死锁。
///
/// 这是收尾回合修掉的死锁形态，且与 `concurrent_adds_cannot_exceed_the_member_limit`
/// 互补：那条用例的组已有 199 名成员，成员行 `FOR UPDATE` 会取到 199 把**记录锁**，
/// 两事务因此天然串行化，压不出缺陷；本条从一个**成员集合为空**的组出发，旧实现里成员行
/// `FOR UPDATE` 在空集上只取到彼此兼容的**间隙锁**，根本没有串行化——两名不同操作者的
/// users 行锁批又不相交（不同操作者、不同目标），于是两事务都读到空名单、都放行、
/// 都去 INSERT，最后在插入意向锁与对方的间隙锁上互相等待，MySQL 牺牲一个返回 40001 → 500。
///
/// 断言直接钉死「响应里不得出现任何非 200」：死锁被折算成 500，一次都不允许。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_adds_of_different_members_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        // 全新空组：成员集合为空，正是间隙锁压不出串行化、旧实现必死锁的初态。
        let group_id = harness::create_group(&app, &admin, "empty_race", "空组并发加").await;
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            0,
            "第 {round} 轮夹具必须从成员集合为空的组出发，否则压不出旧缺陷"
        );

        let left = harness::register_with_code(&app, "race_left", "race_left@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let right = harness::register_with_code(&app, "race_right", "race_right@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        // 操作者必须是两个**不同**账号：同一操作者会让两次请求的 users 行锁批相交、
        // 在锁上被串行化，那样压不出「锁批不相交却都放行」的窗口。
        let left_operator =
            harness::grant_only(&app, &admin, "race_op_left", "access.groups.write").await;
        let right_operator =
            harness::grant_only(&app, &admin, "race_op_right", "access.groups.write").await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (left_operator, "add_group_member", left),
            (right_operator, "add_group_member", right),
        )
        .await;

        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮并发加两名不同成员到空组不得死锁（死锁会被折算成 500），\
             实际响应 {statuses:?}"
        );
        assert_eq!(
            harness::member_rows_of_group(&app, group_id).await,
            2,
            "第 {round} 轮两次都成功时组内必须恰好两名成员"
        );
    }
}

/// 幂等分支不得被上限误拒：重复添加一个**已经在组里**的成员不新增任何人，必须幂等成功。
///
/// 缺陷形态：上限判定 `ensure_member_limit(len + 1)` 排在 `insert_member_in_tx` 的幂等判定
/// **之前**，于是组满 200 人时重复加一个已在组内的成员会被判成「第 201 名」而拒绝
/// （400）——本次请求根本不会新增成员，这是纯粹的误拒。判定口径必须是「本次是否真的会
/// 新增」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn adding_an_existing_member_at_the_limit_stays_idempotent() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "capped", "满员的组").await;
    let target = harness::register_with_code(&app, "cap_target", "cap_target@example.com")
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    // 先填到 199 名占位成员，再经真实 Action 把 target 加成第 200 名（这一名是合法的）。
    let seeded =
        harness::seed_members_directly(&app, group_id, harness::MAX_GROUP_MEMBERS - 1).await;
    assert_eq!(seeded, harness::MAX_GROUP_MEMBERS - 1);
    harness::add_member(&app, &admin, group_id, target).await;
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
        "夹具必须先把组填到恰好上限，且 target 已是成员"
    );
    assert!(harness::is_member(&app, group_id, target).await);

    // 重复添加同一名已成为成员的账号：本次不会新增任何人 → 必须幂等成功。
    let (status, message) = harness::add_member_outcome(&app, &admin, group_id, target).await;
    assert_eq!(
        status, 200,
        "重复添加已在组内的成员不得被成员上限误判为超限（本次不会新增任何成员）：{message}"
    );
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        u64::try_from(harness::MAX_GROUP_MEMBERS).unwrap_or(u64::MAX),
        "幂等分支不得改动成员数"
    );
}

/// spec §13 的扇出锁序测试：**同一组上并发「移出成员」与「加权限」**不得死锁或超时。
///
/// 移出成员会在同一个事务里读全权组成员并逐个锁住成员的 `users` 行（守卫与扇出），
/// 加权限则按升序扇出锁住该组的全部成员——两者必然在同一批用户行上相交。若成员变更
/// 路径在持有成员行锁之后才去取用户行锁、而别的路径以相反顺序取锁，这里就会成环，
/// MySQL 牺牲一个返回 500。本用例钉住「不成环」：两条路径都必须成功。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_member_removal_and_item_add_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let second = harness::add_second_admin(&app, &admin, "admin2").await;
        let third = harness::add_second_admin(&app, &admin, "admin3").await;
        let admin_group = harness::system_admin_group_id(&app).await;
        // 扇出要有真实规模：另两名管理员都在同一个普通组里，给这个组加权限会锁住
        // 「操作者 + 全体成员」三行用户行，正好与移出成员那次事务的用户行锁相交。
        //
        // 刻意**不**把操作者自己写进这个组：入组会递增成员自己的授权版本，操作者的
        // 令牌会随之失效，后续请求会停在 401（与「移出成员」无关的噪声）。操作者
        // 本身是全权组成员，已持有目录里全部权限，因此不加进组也照样能加条目。
        let shared = harness::create_group(&app, &admin, "mixed", "并发锁序组").await;
        harness::add_member(&app, &admin, shared, second.user_id).await;
        harness::add_member(&app, &admin, shared, third.user_id).await;

        let removed = third.user_id;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "remove_group_member",
                    admin_group,
                    removed,
                )
                .await
                .0
            }));
        }
        {
            let app = app.clone();
            let operator = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    shared,
                    "account.users.read",
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮同一组上并发移出成员与加权限不得死锁/超时（死锁会被折算成 500），\
             实际响应 {statuses:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// R3：条目路径与成员路径之间的自提权 TOCTOU（同一账号两个并发请求即可完成）。
// ---------------------------------------------------------------------------

/// spec §8.1 与决策 D2：应用内不存在自提权路径——**并发交错下同样不存在**。
///
/// 缺陷形态：两条路径的判据取自彼此尚未提交的快照，且都是无锁 SELECT：
/// `add_group_item` 的判据是「调用者此刻在该组内」（`list_members_in_tx`），
/// `add_group_member` 的判据是「该组此刻的条目」（`list_items_in_tx`）。
/// 同一账号同时发起这两个请求时：
///
/// - 条目请求读到「我还不在此组」→ 整段 §8.1 子集校验被跳过，权限 X 写进组；
/// - 成员请求读到「该组还没有 X」→ 子集校验通过，把自己写进组。
///
/// 两次提交后，调用者成了「持有一条自己原本没有的权限」的组成员——**不需要同伙**，
/// 一个账号两个并发请求即可完成。串行执行时第二个请求必被 403 拒掉，因此这是纯粹的
/// 原子性缺失，任何单连接串行用例都压不出来。
///
/// 夹具组里有 8 名成员：扇出失效是真的（要逐个锁行并递增授权版本），但刻意**不**做大。
/// 原因是本用例压的是「读快照的时机」而不是「谁跑得慢」：InnoDB 可重复读下两个事务各自
/// 的快照在各自第一条普通读时建立，因此只要两次判据读都早于对方的提交，缺陷就必然出现，
/// 不需要靠人为拖长事务。成员数一旦做得很大，条目请求那批前置行锁本身会把它的快照推到
/// 成员请求提交之后——用例反而会对「操作者行不在锁集内」这种破坏不再敏感（实测：150 名
/// 成员时该破坏仍绿，8 名时变红）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_item_add_and_self_member_add_cannot_self_escalate() {
    const SEEDED_MEMBERS: usize = 8;
    const ESCALATED: &str = "access.grants.write";

    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        // operator 只被直授 access.groups.write：它一旦拿到 access.grants.write，
        // 就只可能来自这次并发，不可能来自任何既有授权。
        let operator = harness::grant_only(&app, &admin, "operator", "access.groups.write").await;
        let group_id = harness::create_group(&app, &admin, "target", "并发自提权目标组").await;
        let seeded = harness::seed_members_directly(&app, group_id, SEEDED_MEMBERS).await;
        assert_eq!(seeded, SEEDED_MEMBERS, "第 {round} 轮夹具必须先把组填满");
        assert!(
            !harness::is_member(&app, group_id, operator.user_id).await,
            "第 {round} 轮夹具必须从「operator 不在组内」出发，否则条目请求的判据会生效"
        );
        assert!(
            !harness::group_holds_item(&app, group_id, ESCALATED).await,
            "第 {round} 轮夹具必须从「组不持有该权限」出发"
        );

        // 两条路径都是授权事实变更。直接并发发出请求，Barrier 对齐请求起点，
        // 两次事务的重叠由真库的独立连接与行锁决定。
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    ESCALATED,
                )
                .await
                .0
            }));
        }
        {
            let app = app.clone();
            let operator = operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_member",
                    group_id,
                    operator.user_id,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }

        assert!(
            statuses.iter().all(|status| *status < 500),
            "第 {round} 轮并发自提权不得泄漏 5xx（含死锁折算），实际 {statuses:?}"
        );
        // 恰好一条成功——两个方向都要钉：
        //   - 两条都 200 ⇒ 两条路径各自读到对方的陈旧快照，完成了自提权（下面那条
        //     `escaped` 断言也会随之变红）；
        //   - 两条都不是 200 ⇒ 两条路径被互相卡死。串行执行时先到者必合法（先加条目
        //     时操作者还不是成员、入组不改变其权限；先入组时组里还没有该条目、入组同样
        //     不改变其权限），后到者才被 §8.1 子集校验拒绝（403），因此 [403,403] 这种
        //     「全都拒掉」的过度串行化同样违反设计意图，`<= 1` 会把它放过去。
        assert_eq!(
            statuses.iter().filter(|status| **status == 200).count(),
            1,
            "第 {round} 轮两条并发路径必须恰好一条成功（另一条被 §8.1 子集校验拒绝），\
             实际 {statuses:?}"
        );
        let escaped = harness::is_member(&app, group_id, operator.user_id).await
            && harness::group_holds_item(&app, group_id, ESCALATED).await;
        assert!(
            !escaped,
            "第 {round} 轮并发让 operator 拿到了它原本没有的权限 {ESCALATED}（响应 {statuses:?}）\
             ——spec §8.1 与决策 D2「应用内不存在自提权路径」被打破"
        );
    }
}

/// spec §13 的扇出锁序测试：**同一组上并发加权限与加成员**不得死锁或超时。
///
/// 加权限会扇出锁住该组的**全部**成员（升序），加成员则锁「操作者 + 目标」两行；
/// 两者都落在 `users` 的行锁上，因此这一次并发真正压的是「不同 Action 的取锁顺序
/// 是否一致」。组里直写 40 名成员让扇出规模真实存在，两名操作者又都是组内成员，
/// 使两组行锁必然相交——不按同一顺序取锁就会在这里成环，MySQL 牺牲一个返回 500。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_item_and_member_add_on_the_same_group_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "fanout", "并发扇出组").await;
        // 扇出要有真实规模：先直写若干成员，让「加权限」那次事务确实要锁住一串用户行。
        harness::seed_members_directly(&app, group_id, 40).await;

        // 两名成员操作者：都在组内（因此两组行锁必然相交），都持 access.groups.write。
        let item_operator =
            harness::grant_only(&app, &admin, "fanout_a", "access.groups.write").await;
        harness::grant_permission(&app, &admin, item_operator.user_id, "access.grants.read").await;
        let member_operator =
            harness::grant_only(&app, &admin, "fanout_b", "access.groups.write").await;
        harness::add_member(&app, &admin, group_id, item_operator.user_id).await;
        harness::add_member(&app, &admin, group_id, member_operator.user_id).await;
        // 授权与入组都递增了授权版本，令牌必须在两者之后重签。
        let item_operator = harness::relogin(&app, &item_operator).await;
        let member_operator = harness::relogin(&app, &member_operator).await;
        let newcomer = harness::register_with_code(&app, "fanout_new", "fanout_new@example.com")
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        // 直接并发发出请求，Barrier 对齐请求起点，两次事务的重叠由真库的独立连接与行锁决定。
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = item_operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                // 操作者已在组内且本来就持有这条权限，§8.1 子集校验放行——本用例测的是锁序，
                // 不是提权判定。
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    "access.grants.read",
                )
                .await
                .0
            }));
        }
        {
            let app = app.clone();
            let operator = member_operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_member",
                    group_id,
                    newcomer,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮同组并发加权限与加成员不得死锁/超时（死锁会被折算成 500），\
             实际 {statuses:?}"
        );
    }
}

/// spec §13 的锁序测试：**同一组、两名成员操作者并发加权限**不得死锁。
///
/// 操作者行锁若写成「先单独锁操作者自己、再按升序锁受影响成员行」，这里必然成环：
/// 两名操作者各持自己的行，再各自去要对方那一行（A 持 A 等 B，B 持 B 等 A），
/// MySQL 会牺牲一个，客户端收到 500。升序才是唯一不会成环的取锁顺序。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn concurrent_item_adds_by_two_member_operators_do_not_deadlock() {
    for round in 0..3 {
        let app = harness::build_test_app().await;
        let admin = harness::bootstrap_admin(&app).await;
        let group_id = harness::create_group(&app, &admin, "shared", "共同维护的组").await;
        // 两名操作者各持 `access.groups.write`（否则调用不了条目 Action）外加一条
        // 它准备写进组的权限（否则会撞上 §8.1 的子集校验）。两条权限必须互不相同，
        // 否则并发插入会撞上 `(group_id, permission)` 唯一键，把锁序问题掩盖成幂等。
        let first = harness::grant_only(&app, &admin, "member_a", "access.groups.write").await;
        harness::grant_permission(&app, &admin, first.user_id, "access.grants.read").await;
        let second = harness::grant_only(&app, &admin, "member_b", "access.groups.write").await;
        harness::grant_permission(&app, &admin, second.user_id, "account.users.read").await;
        harness::add_member(&app, &admin, group_id, first.user_id).await;
        harness::add_member(&app, &admin, group_id, second.user_id).await;
        // 授权与入组都会递增各自的授权版本，令牌必须在两者之后重签。
        let first = harness::relogin(&app, &first).await;
        let second = harness::relogin(&app, &second).await;

        // 直接并发发出请求，Barrier 对齐请求起点，两次事务的重叠由真库的独立连接与行锁决定。
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (operator, permission) in [
            (first.clone(), "access.grants.read"),
            (second.clone(), "account.users.read"),
        ] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_outcome(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    permission,
                )
                .await
                .0
            }));
        }
        let mut statuses = Vec::new();
        for handle in handles {
            statuses.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("任务不应 panic: {error}")),
            );
        }
        assert_eq!(
            statuses,
            vec![200, 200],
            "第 {round} 轮两名成员操作者并发加权限不得死锁/超时，实际 {statuses:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// R5：`permission_group_item.group_id → permission_group.id` 的外键兜底。
// ---------------------------------------------------------------------------

/// spec §8.3：条目表的 `group_id` 必须有真外键，孤儿条目必须被**数据库**拒绝。
///
/// 缺陷形态：表声明里只有 `user_group` 的两条外键（`fk_user_group_user` /
/// `fk_user_group_group`），条目表一条也没有。应用层「删组前先查条目」只是一次无锁
/// SELECT，绕过它（或今后新增的任意写入路径）就能把指向不存在组的条目行落库；
/// 删掉仍有条目的组时也没有数据库兜底。
///
/// 断言刻意分成两半，缺一不可：
/// 1. **反向对照**——同一条 INSERT 打到真实存在的组必须成功。没有这一条，
///    「被拒」可能来自权限字符串 CHECK 或别的约束，读起来像是外键生效了但并不是。
/// 2. **正题**——`group_id` 指向不存在的组时，数据库必须直接以 1452 拒绝。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn an_item_pointing_at_a_missing_group_is_rejected_by_the_database() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "real", "真实组").await;

    // 反向对照：目标组存在时，同样的列口径必须能落库。
    harness::insert_item_row(&app, group_id, "access.grants.read", admin.user_id).await;
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        1,
        "夹具必须先把「正常条目能落库」这件事钉住，否则下面的拒绝无法归因"
    );

    // 正题：目标组不存在。id 由自增主键加上一个远大于当前规模的偏移得到。
    let missing_group_id = group_id + 1_000_000;
    let outcome =
        harness::try_insert_item_row(&app, missing_group_id, "access.grants.read", admin.user_id)
            .await;
    let error = match outcome {
        Ok(()) => panic!(
            "group_id={missing_group_id} 指向不存在的组，条目行却被数据库接受了\
             ——spec §8.3 的外键 permission_group_item.group_id → permission_group.id 缺失"
        ),
        Err(error) => error,
    };
    let code = match &error {
        // `DatabaseError::code()` 给的是 SQLSTATE（23000，整个「完整性约束违规」类），
        // 粒度不够——必须取 MySQL 的错误号才钉得住「是外键拒绝」这件事。
        sqlx::Error::Database(database_error) => database_error
            .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
            .map(sqlx::mysql::MySqlDatabaseError::number),
        other => panic!("期望数据库约束错误（1452），实际 {other:?}"),
    };
    assert_eq!(
        code,
        Some(1452),
        "外键缺失时这条 INSERT 会成功、孤儿条目于是可以落库；实际错误 {error}"
    );

    // 被拒的那一条不得留下任何行。
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        1,
        "被拒的孤儿条目不得落库（组内仍应只有反向对照那一条）"
    );
}

// ---------------------------------------------------------------------------
// G2：管理员等价权限的目录标记与授予闸门。
// ---------------------------------------------------------------------------

/// G2 闸门（路径一）：非全权组成员不得把管理员等价权限直接授予他人。
///
/// 缺陷形态：`grant_permission` 只校验「权限已声明」与「目标账号启用」，对**谁能授予
/// 什么**没有任何限制。于是持 `access.grants.write` 的非管理员可以把
/// `account.users.reset_credentials` 授给任意账号（包括他自己），再借该权限给系统
/// 管理员签发重置凭证、重置其口令并登录成他——闸门缺失时 `access.grants.write` 是
/// 通往 root 的跳板。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_grant_an_admin_equivalent_permission() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 攻击者：只直授 `access.grants.write`（非管理员等价），完全不在全权组内。
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.grants.write").await;
    // 目标：普通账号，此刻只直授了一条非管理员等价权限。
    let target = harness::grant_only(&app, &admin, "target", "feishu.datasource.read").await;

    let status = harness::grant_permission_status(
        &app,
        &outsider,
        target.user_id,
        "account.users.reset_credentials",
    )
    .await;
    assert_eq!(
        status, 403,
        "非全权组成员不得授予管理员等价权限，必须 403 PermissionDenied"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target.user_id).await,
        1,
        "被拒的授予不得落库（目标应仍只有夹具给的那一条 feishu.datasource.read）"
    );
}

/// G2 闸门（路径二）：非全权组成员不得把管理员等价权限加进组。
///
/// 与路径一的差别在于授权事实换成了组条目：闸门若只看「直接授予」，任何人都能先建
/// 一个组、把管理员等价权限写进条目，再把同伙拉进组绕过它。调用者刻意不是该组成员，
/// 以排除 §8.1 自提权判定先把它拦下的可能——那样这条用例就测不到 G2 闸门。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_add_an_admin_equivalent_permission_to_a_group() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.groups.write").await;
    let group_id = harness::create_group(&app, &admin, "ops", "运营组").await;
    assert!(
        !harness::is_member(&app, group_id, outsider.user_id).await,
        "调用者必须不在目标组内，否则 §8.1 自提权判定会先拦下，用例测不到 G2 闸门"
    );

    let status = harness::add_group_item_status(
        &app,
        &outsider,
        group_id,
        "account.users.reset_credentials",
    )
    .await;
    assert_eq!(
        status, 403,
        "非全权组成员不得把管理员等价权限加进组，必须 403 PermissionDenied"
    );
    assert!(
        !harness::group_holds_item(&app, group_id, "account.users.reset_credentials").await,
        "被拒的条目不得落库"
    );
}

/// G2 闸门（路径三）：非全权组成员不得把用户加进一个**已含**管理员等价权限的组。
///
/// 这是最容易漏的一条：组本身没有任何变化，变的是成员——「加成员」会把组的全部权限
/// （含管理员等价那条）转授给新成员。只看「本次请求里带了哪条权限」的闸门会放它过去。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_add_a_member_to_a_group_holding_admin_equivalent_permissions() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.groups.write").await;
    let target = harness::grant_only(&app, &admin, "target", "feishu.datasource.read").await;
    // 前置：由全权组成员把管理员等价权限写进组（这条操作本身必须放行）。
    let group_id = harness::create_group(&app, &admin, "credential_ops", "凭据运营组").await;
    assert_eq!(
        harness::add_group_item_status(&app, &admin, group_id, "account.users.reset_credentials")
            .await,
        200,
        "夹具前置：全权组成员必须能把管理员等价权限加进组"
    );

    let status = harness::add_member_status(&app, &outsider, group_id, target.user_id).await;
    assert_eq!(
        status, 403,
        "非全权组成员不得把用户加进持有管理员等价权限的组，必须 403 PermissionDenied"
    );
    assert!(
        !harness::is_member(&app, group_id, target.user_id).await,
        "被拒的成员写入不得落库"
    );
}

/// G2 闸门的活性对照：全权组成员执行三条路径必须全部放行。
///
/// 判据是「调用者是否为全权组成员」，不是「调用者是否持有该权限」——内置全权组的成员
/// 天然持有全部权限（含管理员等价权限），闸门不得把他们一并锁死，否则系统管理员再也
/// 无法委派凭据类权限，引导后立刻变成不可运维。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_system_admin_can_grant_admin_equivalent_permissions_and_membership() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let target = harness::grant_only(&app, &admin, "target", "feishu.datasource.read").await;

    // 路径一：直接授予。
    assert_eq!(
        harness::grant_permission_status(
            &app,
            &admin,
            target.user_id,
            "account.users.reset_credentials"
        )
        .await,
        200,
        "全权组成员必须能直接授予管理员等价权限"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target.user_id).await,
        2,
        "授予必须真的落库（夹具原一条 + 新增一条）"
    );

    // 路径二：加进组。
    let group_id = harness::create_group(&app, &admin, "credential_ops", "凭据运营组").await;
    assert_eq!(
        harness::add_group_item_status(&app, &admin, group_id, "account.users.reset_credentials")
            .await,
        200,
        "全权组成员必须能把管理员等价权限加进组"
    );
    assert!(harness::group_holds_item(&app, group_id, "account.users.reset_credentials").await);

    // 路径三：把成员加进已含管理员等价权限的组。
    let other = harness::grant_only(&app, &admin, "other", "feishu.datasource.read").await;
    assert_eq!(
        harness::add_member_status(&app, &admin, group_id, other.user_id).await,
        200,
        "全权组成员必须能把用户加进持有管理员等价权限的组"
    );
    assert!(harness::is_member(&app, group_id, other.user_id).await);
}

/// 权限目录必须把危害面发出来：每条管理员等价权限带标记，非管理员等价的不带。
///
/// 用例把「带标记的权限集合」与测试侧独立复述的期望值**整体比对**，因此它同时钉住
/// 两件事：(1) 运行期真的按清单打了标记；(2) 清单里每条都仍是当前 Catalog 已声明的
/// 权限——权限改名后它不会出现在目录里，集合随之少一项而失败，清单无法静默失效。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn the_permission_catalog_marks_exactly_the_admin_equivalent_permissions() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;

    let mut flagged: Vec<String> = harness::list_permission_entries(&app, &admin)
        .await
        .iter()
        .filter(|entry| entry["admin_equivalent"].as_bool() == Some(true))
        .map(|entry| {
            entry["permission"]
                .as_str()
                .unwrap_or_else(|| panic!("目录条目缺少 permission 字符串: {entry}"))
                .to_string()
        })
        .collect();
    flagged.sort();
    let expected: Vec<String> = ADMIN_EQUIVALENT_PERMISSIONS
        .iter()
        .map(|permission| permission.to_string())
        .collect();
    assert_eq!(
        flagged, expected,
        "目录标记的管理员等价集合必须恰好等于清单；少一项说明清单里的权限已不在 Catalog"
    );

    // 反向对照：运营/读取类权限一律不得被标成管理员等价——标记一旦过宽，
    // 闸门会把正常委派一并锁死。
    for permission in [
        "access.grants.write",
        "access.groups.write",
        "account.users.manage",
        "feishu.datasource.read",
    ] {
        assert_eq!(
            harness::catalog_flag_of(&app, &admin, permission).await,
            Some(false),
            "{permission} 不是管理员等价权限，不得被标记"
        );
    }
}

// ---------------------------------------------------------------------------
// H1：保留 group_key 在创建路径上的硬拒绝，及「全权组伪造」所依赖的 admin-only 守卫。
// ---------------------------------------------------------------------------

/// 保留 `group_key` 必须在**创建路径**上被硬拒绝，而不是靠别处的守卫兜住。
///
/// 缺陷形态：`create_group` 不保留任何 key，而 `resolve_group_permissions` 判断一个组
/// 是否全权**只看 `group_key == "system_admin"`**（命中即返回整个权限目录）。正常引导后
/// 该 key 已在库中，重复建组会撞唯一键，因此这条路径只在**内置组不在库中**时可达——即
/// 设计 §7.3 的灾备态（哨兵/数据被误删、或按运维 SQL 手工重建的窗口）。此时任何持
/// `access.groups.write` 的账号都能建出一个同名的**空组**，它一经解析就等价于整个目录。
///
/// 用例用直写复现那个窗口（[`harness::drop_builtin_admin_group`]）——只有在同名 key
/// 不存在时，这次创建才可能成功，缺陷版本因此必然变红。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn create_group_rejects_the_reserved_system_admin_key() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 攻击者：只直授 `access.groups.write`，完全不在全权组内。
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.groups.write").await;
    // 复现 §7.3 的零管理员窗口：内置全权组当前不在库中。
    harness::drop_builtin_admin_group(&app).await;
    assert!(
        !harness::group_exists(&app, SYSTEM_ADMIN_GROUP_KEY).await,
        "夹具必须先复现同名 key 不存在的状态，否则唯一键会替实现挡下这次创建，用例测不到防线"
    );

    let (status, message) =
        harness::create_group_outcome(&app, &outsider, SYSTEM_ADMIN_GROUP_KEY, "伪造的全权组")
            .await;
    assert_eq!(
        status, 400,
        "保留 key 必须以 ParamInvalid→400 被拒绝，实际 {status}: {message}"
    );
    assert!(
        message.contains("保留"),
        "拒绝信息必须说明该 key 被保留，实际 {message}"
    );
    assert!(
        !harness::group_exists(&app, SYSTEM_ADMIN_GROUP_KEY).await,
        "被拒的创建不得落库：库中不得出现解析为整个目录的组"
    );
    assert_eq!(
        harness::member_rows_of_group_key(&app, SYSTEM_ADMIN_GROUP_KEY).await,
        0,
        "被拒的创建不得留下任何成员行"
    );
}

/// 反向对照：普通 key 建组必须仍然成功。
///
/// 没有这一条，`create_group` 的保留 key 判定即便被写成「一律拒绝」，上面那条用例也照样
/// 会绿——拒绝面过宽同样是故障，会把正常建组能力整个砍掉。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn create_group_still_accepts_an_ordinary_key() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.groups.write").await;

    let (status, message) = harness::create_group_outcome(&app, &outsider, "ops", "运营组").await;
    assert_eq!(
        status, 200,
        "普通 key 建组必须成功，实际 {status}: {message}"
    );
    assert!(harness::group_exists(&app, "ops").await, "建组必须真的落库");
    assert_eq!(
        harness::member_rows_of_group_key(&app, "ops").await,
        0,
        "刚建的组不该有任何成员行"
    );
}

/// 显式化「暗门不可利用」所依赖的那条隐式假设：`add_group_member` 的 admin-only 守卫
/// 的**加入方向**。
///
/// 缺陷形态：该守卫（`add_group_member.rs`「只有全权组成员能修改全权组成员」）此前只有
/// 「移出」方向有用例（见 `a_non_admin_cannot_remove_members_of_the_system_admin_group`），
/// 「加入」方向零覆盖。而保留 key 若能从创建路径写出（见上一条用例），这个空组能否被
/// 利用来提权**完全**取决于这条守卫——它挡不住的话，非全权组成员就能把自己或同伙塞进
/// 那个解析为整个目录的组。
///
/// 判据是「调用者是否为该组成员」，与保留 key 拒绝是两条独立防线；两条都在，公开 API
/// 才无法造出解析为整个目录的组。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_non_admin_cannot_add_members_to_the_system_admin_group() {
    let app = harness::build_test_app().await;
    let first = harness::bootstrap_admin(&app).await;
    // 第二名管理员让夹具从一个真实的「两名启用管理员」全权组出发。
    harness::add_second_admin(&app, &first, "admin2").await;
    // 攻击者：只直授 `access.groups.write`，完全不在全权组内。
    let outsider = harness::grant_only(&app, &first, "outsider", "access.groups.write").await;
    let target = harness::register_with_code(&app, "target", "target@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册加成员目标失败: {error}"));
    let group_id = harness::system_admin_group_id(&app).await;
    let before = harness::member_rows_of_group(&app, group_id).await;
    assert_eq!(before, 2, "夹具必须从两名管理员出发");

    // 加**自己**：§8.1 附加规则的 admin-only 守卫直接拒绝。
    let self_status = harness::add_member_status(&app, &outsider, group_id, outsider.user_id).await;
    assert_eq!(
        self_status, 403,
        "非全权组成员不得把自己加进全权组，必须 403 PermissionDenied"
    );
    assert!(
        !harness::is_member(&app, group_id, outsider.user_id).await,
        "被拒的自我加入不得落成员行"
    );

    // 加**他人**：这条路径不触发 §8.1 自提权判定（改动的是他人权限），能否被拦下
    // **只**取决于 admin-only 守卫——正是「暗门不可利用」所依赖的那条判据。
    let other_status = harness::add_member_status(&app, &outsider, group_id, target).await;
    assert_eq!(
        other_status, 403,
        "非全权组成员不得把他人加进全权组，必须 403 PermissionDenied"
    );
    assert!(
        !harness::is_member(&app, group_id, target).await,
        "被拒的加入不得落成员行"
    );

    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        before,
        "被拒的两次加入都不得改变成员行数"
    );
}

// ---------------------------------------------------------------------------
// 错误码表补齐：组不存在/目标用户不存在→404、group_key 重复→400、
// 内置 system_admin 组被改/删/增删条目→400（docs/contracts/AUTHZ_GRANTS.md
// §错误码表），以及 revoke_permission / list_user_grants 两个零覆盖接口与
// 框架级缺权限 403 的端到端断言。
// ---------------------------------------------------------------------------

/// 错误码表第一行：`add_group_member` 目标组不存在必须折算成 404。
///
/// 缺陷形态：组不存在若被漏折算（或折算成 500/别的状态），客户端无法区分
/// 「组已被删」与「服务端故障」。契约钉死 `RecordNotFound`→404；实现侧
/// （`add_group_member.rs`）在锁组行时判定，外键兜底同一支。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn add_group_member_rejects_a_missing_group_with_404() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "real", "真实组").await;
    let member = harness::register_with_code(&app, "member", "member@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册目标用户失败: {error}"));

    // 组 id 由自增主键分配，真实组 id 加一个远大于当前规模的偏移必然不存在
    // （与 `an_item_pointing_at_a_missing_group_is_rejected_by_the_database`
    // 同款取法）。
    let missing_group_id = group_id + 1_000_000;
    let (status, message) =
        harness::add_member_outcome(&app, &admin, missing_group_id, member).await;
    assert_eq!(
        status, 404,
        "目标组不存在必须 RecordNotFound→404，实际 {status}: {message}"
    );
    assert!(
        message.contains("记录未找到"),
        "拒绝信息必须是记录未找到语义，实际 {message}"
    );
}

/// 错误码表第二行：`add_group_member` 目标用户不存在必须折算成 404。
///
/// 契约钉死 `UserNotFound`→404；实现侧（`add_group_member.rs`）在开事务前按
/// 用户版本快照判存在性，因此这里只可能是 404，绝不会落到外键兜底。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn add_group_member_rejects_a_missing_target_user_with_404() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "real", "真实组").await;

    let missing_user = 999_999;
    let (status, message) = harness::add_member_outcome(&app, &admin, group_id, missing_user).await;
    assert_eq!(
        status, 404,
        "目标用户不存在必须 UserNotFound→404，实际 {status}: {message}"
    );
    assert!(
        message.contains("用户未找到"),
        "拒绝信息必须点名用户未找到，实际 {message}"
    );
    assert_eq!(
        harness::member_rows_of_group(&app, group_id).await,
        0,
        "被拒的加成员不得落任何成员行"
    );
}

/// 错误码表第二行的另一条接口：`list_user_grants` 目标用户不存在必须折算成 404。
///
/// 这是 `list_user_grants` 在本文件里第一次被端到端调用，同时钉住「user_id 走
/// 路径参数」的形态（写进 body 会被判为缺参，见 `dispatch_with_path` 的说明）。
/// 契约钉死 `UserNotFound`→404（`list_user_grants.rs` 按版本快照判存在性）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn list_user_grants_rejects_a_missing_target_user_with_404() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;

    let missing_user = 999_999;
    let (status, message) = harness::list_user_grants_outcome(&app, &admin, missing_user).await;
    assert_eq!(
        status, 404,
        "目标用户不存在必须 UserNotFound→404，实际 {status}: {message}"
    );
    assert!(
        message.contains("用户未找到"),
        "拒绝信息必须点名用户未找到，实际 {message}"
    );
}

/// 错误码表第三行：`group_key` 撞唯一键必须 400。
///
/// 契约（docs/contracts/AUTHZ_GRANTS.md §错误码表第三行）钉死：`group_key` 重复 →
/// `ParamInvalid("group_key")` → **400**。框架层（yang-base `table/table_query/write.rs`
/// 的 `insert_returning_id_in_tx`）用 `.map_err(BaseError::DatabaseExecuteFailed)` 直包、
/// 绕过 `From<DbError>` 的唯一键特判，因此 `create_group.rs` 在应用侧做了临时折算
/// （认报文中的唯一索引名 `uk_permission_group_key`，见该文件的
/// `fold_duplicate_group_key`）。本用例钉住折算后的契约形态：400 + 可归因文案，
/// 且不泄漏 Duplicate entry 数据库报文；框架侧修好后本用例仍须保持通过。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn create_group_rejects_a_duplicate_group_key_with_param_invalid_400() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;

    let (status1, message1) = harness::create_group_outcome(&app, &admin, "ops", "运营组").await;
    assert_eq!(status1, 200, "首次建组必须成功，实际 {status1}: {message1}");

    let (status2, message2) = harness::create_group_outcome(&app, &admin, "ops", "重复组").await;
    assert_eq!(
        status2, 400,
        "重复 group_key 必须 ParamInvalid→400（契约 AUTHZ_GRANTS.md 错误码表第三行），实际 {status2}: {message2}"
    );
    assert!(
        message2.contains("该组标识已被占用"),
        "400 必须给出可归因文案，实际 {message2}"
    );
    assert!(
        message2.contains("group_key"),
        "400 必须点名 group_key，实际 {message2}"
    );
    assert!(
        !message2.contains("Duplicate entry"),
        "400 不得泄漏数据库唯一键报文，实际 {message2}"
    );
    assert_eq!(
        harness::group_rows_of_key(&app, "ops").await,
        1,
        "被拒的重复创建不得留下第二行组事实"
    );
}

/// 错误码表第五行：内置 `system_admin` 组被改/删/增删条目一律 400。
///
/// 契约钉死 `ParamInvalid("group_id")`→400：内置组的权限由权限目录计算、展示
/// 信息由引导流程写定，任何改写路径都不该存在第二口径。反向对照打在普通组上，
/// 拒绝面过宽同样是故障。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn builtin_system_admin_group_is_immune_to_update_delete_and_item_changes() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::system_admin_group_id(&app).await;

    let (status, message) = harness::update_group_outcome(&app, &admin, group_id, "改名").await;
    assert_eq!(
        status, 400,
        "改内置组展示信息必须被拒，实际 {status}: {message}"
    );
    assert!(
        message.contains("内置"),
        "拒绝信息必须点名内置组，实际 {message}"
    );

    let status = harness::delete_group_status(&app, &admin, group_id).await;
    assert_eq!(status, 400, "删内置组必须被拒，实际 {status}");

    let status = harness::add_group_item_status(&app, &admin, group_id, "access.grants.read").await;
    assert_eq!(status, 400, "给内置组加条目必须被拒，实际 {status}");

    let status =
        harness::remove_group_item_status(&app, &admin, group_id, "access.grants.read").await;
    assert_eq!(status, 400, "从内置组移条目必须被拒，实际 {status}");

    assert!(
        harness::group_exists_by_id(&app, group_id).await,
        "四次被拒的调用不得让内置组消失"
    );

    // 反向对照：同一批操作打在普通组上必须成功——拒绝面过宽同样是故障。
    // 删除放在最后：普通组没有成员，不会先撞上「组内非空」守卫（该守卫另有用例
    // `deleting_a_group_with_members_is_rejected` 钉住）。
    let normal_id = harness::create_group(&app, &admin, "ops", "运营组").await;
    let (status, message) = harness::update_group_outcome(&app, &admin, normal_id, "改名").await;
    assert_eq!(
        status, 200,
        "普通组必须能改展示信息，实际 {status}: {message}"
    );
    let status =
        harness::add_group_item_status(&app, &admin, normal_id, "access.grants.read").await;
    assert_eq!(status, 200, "普通组必须能加条目，实际 {status}");
    let status =
        harness::remove_group_item_status(&app, &admin, normal_id, "access.grants.read").await;
    assert_eq!(status, 200, "普通组必须能移条目，实际 {status}");
    let status = harness::delete_group_status(&app, &admin, normal_id).await;
    assert_eq!(status, 200, "空普通组必须能删除，实际 {status}");
    assert!(
        !harness::group_exists_by_id(&app, normal_id).await,
        "删除必须真的落库，组行不得残留"
    );
}

/// `revoke_permission` 的端到端覆盖：撤销直授事实、令牌立即失效、幂等。
///
/// 契约（AUTHZ_GRANTS.md §写入一致性）：撤销必须在同一事务里删事实行 + 递增授权
/// 版本 + 追加 Outbox，并由 `revoke_by_subject` 立即收敛 Redis 水位线——旧 Access
/// Token 必须在撤销后**立刻**失效，而不是等 Outbox 异步传播。重复撤销幂等
/// （`changed: false`），不再递增版本。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn revoke_permission_removes_the_grant_stales_the_token_and_stays_idempotent() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let holder = harness::grant_only(&app, &admin, "revokee", "access.grants.read").await;
    assert_eq!(
        harness::grant_rows_of_user(&app, holder.user_id).await,
        1,
        "夹具必须先造出一条直授事实"
    );
    let version_before = harness::authz_version_of(&app, holder.user_id).await;

    let (status, message) =
        harness::revoke_permission_outcome(&app, &admin, holder.user_id, "access.grants.read")
            .await;
    assert_eq!(status, 200, "撤销必须成功，实际 {status}: {message}");
    assert!(
        message.contains("已撤销"),
        "成功文案必须说明权限已撤销，实际 {message}"
    );

    // 事实行删除 + 授权版本递增（写入一致性契约的三个动作）。
    assert_eq!(
        harness::grant_rows_of_user(&app, holder.user_id).await,
        0,
        "撤销后直授事实行必须消失"
    );
    assert_eq!(
        harness::authz_version_of(&app, holder.user_id).await,
        version_before + 1,
        "撤销必须单调递增授权版本"
    );
    // 旧令牌立即失效：撤销路径经 `revoke_by_subject` 把令牌标为已撤销（TokenRevoked），
    // 比版本水位线（AuthorizationStale）更强的即时收敛，不等 Outbox 异步传播。
    assert!(
        harness::member_token_is_dead(&app, &holder.token).await,
        "撤销后旧 Access Token 必须立即不可用"
    );

    // 幂等：重复撤销仍成功，但不再改变任何事实（版本不得再增）。
    let (status, message) =
        harness::revoke_permission_outcome(&app, &admin, holder.user_id, "access.grants.read")
            .await;
    assert_eq!(
        status, 200,
        "重复撤销必须幂等成功，实际 {status}: {message}"
    );
    assert!(
        message.contains("本就没有该权限"),
        "幂等文案必须说明目标本就没有该权限，实际 {message}"
    );
    assert_eq!(
        harness::authz_version_of(&app, holder.user_id).await,
        version_before + 1,
        "幂等撤销不得再次递增授权版本"
    );
}

/// `list_user_grants` 的端到端覆盖：返回目标用户的全部直授权限。
///
/// 契约（AUTHZ_GRANTS.md 管理接口表）：`GET /api/v1/access/users/{user_id}/grants`
/// 只需 `access.grants.read`。断言按权限名排序后整体比对，钉住
/// 响应形态与字段名；零直授账号必须返回空数组而不是缺字段。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn list_user_grants_returns_the_direct_grants_of_the_target_user() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let holder = harness::grant_only(&app, &admin, "granteeb", "access.grants.read").await;
    harness::grant_permission(&app, &admin, holder.user_id, "access.grants.write").await;

    let response = harness::list_user_grants(&app, &admin, holder.user_id)
        .await
        .unwrap_or_else(|error| panic!("查询用户授权失败: {error}"));
    assert_eq!(
        response.code, 0,
        "查询用户授权必须成功: {}",
        response.message
    );
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("查询响应缺少 data: {response:?}"));
    assert_eq!(
        data["user_id"].as_i64(),
        Some(holder.user_id),
        "响应必须回显目标 user_id"
    );
    let mut got: Vec<String> = data["grants"]
        .as_array()
        .unwrap_or_else(|| panic!("grants 必须是数组: {data}"))
        .iter()
        .map(|grant| {
            grant["permission"]
                .as_str()
                .unwrap_or_else(|| panic!("条目缺少 permission 字符串: {grant}"))
                .to_string()
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            "access.grants.read".to_string(),
            "access.grants.write".to_string()
        ],
        "必须恰好返回该用户的全部直授权限"
    );

    // 反向对照：无任何直授的账号返回空数组（不是缺字段、更不是报错）。
    let empty_user = harness::register_with_code(&app, "no_grants", "no_grants@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册零直授账号失败: {error}"));
    let response = harness::list_user_grants(&app, &admin, empty_user)
        .await
        .unwrap_or_else(|error| panic!("查询零直授账号失败: {error}"));
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("查询响应缺少 data: {response:?}"));
    assert_eq!(
        data["grants"].as_array().map(Vec::len),
        Some(0),
        "零直授账号必须返回空数组，实际 {data}"
    );
}

/// 框架级缺权限判定：持 A 权限调需 B 权限的 Action 必须 403 PermissionDenied。
///
/// 既有 403 用例全部是**业务层**拒绝（自提权 / 全权组成员守卫 / 管理员等价闸门），
/// 框架中间件的「Action 声明权限 ⊄ 调用者持有权限 → PermissionDenied(403)」这条
/// 链路此前零端到端断言。`access.groups` 域授权判定下沉到 handler 内实现后（见
/// `groups/actions/*` 的 register 注释），**只有 `access.grants` 域仍以
/// `.permissions(...)` 声明权限键**，因此这条链路改在 grants 域取证（契约
/// AUTHZ_GRANTS.md 管理接口表）：`list_user_grants` 需要 `access.grants.read`、
/// `grant_permission` 需要 `access.grants.write`。
///
/// `access.groups` 的新语义（登录即可建组 + 组级可见性）在同一用例后半段钉住：
/// 无全局权限的账号建组必须成功（建完即组所有者）、列表只见自己的组；持全局读
/// 权限的账号才看得到全部组。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_user_without_the_required_permission_is_rejected_with_403() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 攻击者：只直授 access.groups.read，完全没有 grants 域权限。
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.groups.read").await;
    let authorization = format!("Bearer {}", outsider.token);

    // 读 Action：持 access.groups.read 调需 access.grants.read 的 list_user_grants。
    match dispatch(
        &app,
        "access.grants",
        "list_user_grants",
        json!({}),
        &[("authorization", authorization.as_str())],
        52_400,
    )
    .await
    {
        Err(BaseError::PermissionDenied(message)) => assert!(
            message.contains("access.grants.read"),
            "拒绝信息必须点名缺失的权限，实际 {message}"
        ),
        Ok(response) => panic!(
            "缺权限调用必须被拒，实际成功（code={}，{}）",
            response.code, response.message
        ),
        Err(other) => panic!("缺权限必须 PermissionDenied→403，实际 {other}"),
    }

    // 写 Action 同一判据：持 access.groups.read 调需 access.grants.write 的
    // grant_permission——仍被权限中间件拒绝。
    let target = harness::register_with_code(&app, "target", "target@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 target 失败: {error}"));
    let status =
        harness::grant_permission_status(&app, &outsider, target, "feishu.datasource.read").await;
    assert_eq!(
        status, 403,
        "缺 access.grants.write 授予必须 403 PermissionDenied，实际 {status}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target).await,
        0,
        "被拒的授予不得落库"
    );

    // 反向对照：持 access.grants.read 的账号必须能查授权。
    let reader = harness::grant_only(&app, &admin, "reader", "access.grants.read").await;
    let response = harness::list_user_grants(&app, &reader, reader.user_id)
        .await
        .unwrap_or_else(|error| panic!("持权限查询授权失败: {error}"));
    assert_eq!(
        response.code, 0,
        "持 access.grants.read 必须能查授权: {}",
        response.message
    );

    // ---- access.groups 的新语义（handler 内判定）：登录即可建组 + 组级可见性 ----
    // 前置组：管理员建一个「admin_group」，与后面的零权限用户无关（非其所有者/成员）。
    let admin_group_id = harness::create_group(&app, &admin, "admin_group", "管理员的组").await;

    // 无 access.groups.read 的旁观者建组必须成功（建完即组所有者；管理权由所有者
    // 身份或全局写权限带来——create_group 的 register 注释「任何登录用户可建组」）。
    let bystander_id = harness::register_with_code(&app, "bystander", "bystander@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 bystander 失败: {error}"));
    let bystander = harness::Admin {
        username: "bystander".to_string(),
        user_id: bystander_id,
        token: harness::login_as(&app, "bystander").await,
    };
    let (status, message) =
        harness::create_group_outcome(&app, &bystander, "own", "自己的组").await;
    assert_eq!(
        status, 200,
        "任何登录用户必须能建组（建完即所有者），实际 {status}: {message}"
    );
    assert!(harness::group_exists(&app, "own").await, "建组必须真的落库");

    // 可见性：无 access.groups.read 的账号列表只见自己的组——管理员的组与内置
    // 全权组都不可见（不是其所有者/成员）。
    assert_eq!(
        harness::list_group_keys(&app, &bystander).await,
        ["own"],
        "无全局读权限的账号只能看到自己的组"
    );

    // get_group 对「不可见」与「不存在」统一 404（RecordNotFound 同形态）：
    // 无 read 的 bystander 查别人的组，与查一个不存在的组必须返回完全相同的
    // （状态码, 消息），消除「自增 group_id 可遍历枚举组是否存在」的侧信道。
    let hidden = harness::get_group_outcome(&app, &bystander, admin_group_id).await;
    let missing = harness::get_group_outcome(&app, &bystander, admin_group_id + 1_000_000).await;
    assert_eq!(
        hidden.0, 404,
        "不可见的组必须与不存在的组同形态 404，实际 {}: {}",
        hidden.0, hidden.1
    );
    assert_eq!(
        hidden, missing,
        "「不可见」与「不存在」的响应必须逐字节同形态（不得泄漏组是否存在）"
    );

    // 反向对照：持 access.groups.read 的账号必须看到全部组（含内置全权组）。
    let outsider_keys = harness::list_group_keys(&app, &outsider).await;
    for key in ["system_admin", "admin_group", "own"] {
        assert!(
            outsider_keys.iter().any(|k| k == key),
            "持 access.groups.read 必须能看到 {key}，实际 {outsider_keys:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 直授批量与过期语义：batch_grant_permissions / batch_revoke_permissions /
// expires_at 的端到端用例（核心任务「直授批量/过期」的集成覆盖）。
// ---------------------------------------------------------------------------

/// 批量授予的端到端覆盖：多用户 × 多权限一次落库、计数正确、幂等重复全跳过。
///
/// 契约（AUTHZ_GRANTS.md 批量授予）：事务原子、同 user_id 多条合并为一次版本递增、
/// 已持有有效直授的条目幂等跳过。断言分四段：响应计数 → Token claims → 数据库
/// 事实（行数与版本）→ 重复调用幂等。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn batch_grant_permissions_writes_every_item_and_repeats_idempotently() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let alice = harness::register_with_code(&app, "alice", "alice@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 alice 失败: {error}"));
    let bob = harness::register_with_code(&app, "bob", "bob@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 bob 失败: {error}"));

    let items = [
        harness::grant_item(alice, "access.grants.read"),
        harness::grant_item(alice, "access.grants.write"),
        harness::grant_item(bob, "access.groups.read"),
        harness::grant_item(bob, "feishu.datasource.read"),
    ];
    let version_alice_before = harness::authz_version_of(&app, alice).await;
    let response = harness::batch_grant_permissions(&app, &admin, &items)
        .await
        .unwrap_or_else(|error| panic!("批量授予失败: {error}"));
    assert_eq!(response.code, 0, "批量授予必须成功: {}", response.message);
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("批量授予响应缺少 data: {response:?}"));
    assert_eq!(
        data["succeeded"].as_u64(),
        Some(4),
        "四条款目必须全部实际写入: {data}"
    );
    assert_eq!(
        data["skipped"].as_u64(),
        Some(0),
        "首次批量不得跳过: {data}"
    );
    assert_eq!(
        data["failed"].as_array().map(Vec::len),
        Some(0),
        "成功响应的 failed 明细恒为空: {data}"
    );

    // 数据库事实：两名用户各落两行直授；alice 的两条写入合并为一次版本递增（+1 而非 +2）。
    assert_eq!(
        harness::grant_rows_of_user(&app, alice).await,
        2,
        "alice 应恰好两行直授"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, bob).await,
        2,
        "bob 应恰好两行直授"
    );
    assert_eq!(
        harness::authz_version_of(&app, alice).await,
        version_alice_before + 1,
        "同 user_id 多条写入必须合并为一次授权版本递增"
    );

    // Token claims：批量授予后签发的新令牌必须含全部新权限（AuthzGrantResolver 注入）。
    let alice_permissions = token_permissions(&harness::login_as(&app, "alice").await)
        .unwrap_or_else(|error| panic!("校验 alice Token claims 失败: {error}"));
    for permission in ["access.grants.read", "access.grants.write"] {
        assert!(
            alice_permissions.iter().any(|p| p == permission),
            "alice 的新令牌必须含 {permission}，实际 {alice_permissions:?}"
        );
    }
    let bob_permissions = token_permissions(&harness::login_as(&app, "bob").await)
        .unwrap_or_else(|error| panic!("校验 bob Token claims 失败: {error}"));
    for permission in ["access.groups.read", "feishu.datasource.read"] {
        assert!(
            bob_permissions.iter().any(|p| p == permission),
            "bob 的新令牌必须含 {permission}，实际 {bob_permissions:?}"
        );
    }

    // 幂等重复调用：目标已持有全部有效直授 → 全部跳过，不得再写行、不得再动版本。
    let version_alice_after = harness::authz_version_of(&app, alice).await;
    let response = harness::batch_grant_permissions(&app, &admin, &items)
        .await
        .unwrap_or_else(|error| panic!("幂等重放批量授予失败: {error}"));
    assert_eq!(response.code, 0, "幂等重放必须成功: {}", response.message);
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("幂等重放响应缺少 data: {response:?}"));
    assert_eq!(
        data["succeeded"].as_u64(),
        Some(0),
        "已持有的条目不得重复写入: {data}"
    );
    assert_eq!(
        data["skipped"].as_u64(),
        Some(4),
        "已持有的条目必须全部计入幂等跳过: {data}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, alice).await,
        2,
        "幂等重放不得新增行"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, bob).await,
        2,
        "幂等重放不得新增行"
    );
    assert_eq!(
        harness::authz_version_of(&app, alice).await,
        version_alice_after,
        "幂等跳过不得递增授权版本"
    );
}

/// 批量撤销的端到端覆盖：事实行删除、按用户合并版本递增、旧令牌失效、幂等重放。
///
/// 与单条撤销（`revoke_permission_removes_the_grant_stales_the_token_and_stays_idempotent`）
/// 的差别：批量撤销没有 `revoke_by_subject` 即时收敛（批量版本只经 Outbox Worker
/// 异步发布），旧令牌失效走「缓存键过期后回查主库」的常规窗口，与
/// `admin_can_remove_themselves_when_another_admin_remains` 同款等待；
/// 新签发的令牌则立即不含被撤销权限。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn batch_revoke_permissions_removes_rows_bumps_version_once_and_stales_old_tokens() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let holder = harness::register_with_code(&app, "holder", "holder@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 holder 失败: {error}"));

    // 夹具：批量授予三条直授，随后签发令牌（claims 快照含三条）。
    let grant_items = [
        harness::grant_item(holder, "access.grants.read"),
        harness::grant_item(holder, "access.grants.write"),
        harness::grant_item(holder, "access.groups.read"),
    ];
    harness::batch_grant_permissions(&app, &admin, &grant_items)
        .await
        .unwrap_or_else(|error| panic!("批量授予夹具失败: {error}"));
    assert_eq!(
        harness::grant_rows_of_user(&app, holder).await,
        3,
        "夹具必须先造出三条直授"
    );
    let holder_token = harness::login_as(&app, "holder").await;
    let version_before = harness::authz_version_of(&app, holder).await;

    // 批量撤销其中两条。
    let revoke_items = [
        harness::revoke_item(holder, "access.grants.read"),
        harness::revoke_item(holder, "access.groups.read"),
    ];
    let response = harness::batch_revoke_permissions(&app, &admin, &revoke_items)
        .await
        .unwrap_or_else(|error| panic!("批量撤销失败: {error}"));
    assert_eq!(response.code, 0, "批量撤销必须成功: {}", response.message);
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("批量撤销响应缺少 data: {response:?}"));
    assert_eq!(
        data["succeeded"].as_u64(),
        Some(2),
        "两条款目必须实际删除: {data}"
    );
    assert_eq!(
        data["skipped"].as_u64(),
        Some(0),
        "首次批量不得跳过: {data}"
    );
    assert_eq!(
        data["failed"].as_array().map(Vec::len),
        Some(0),
        "成功响应的 failed 明细恒为空: {data}"
    );

    // 事实层：行删除 + 版本恰好 +1（同 user 两条删除合并，而非 +2）。
    assert_eq!(
        harness::grant_rows_of_user(&app, holder).await,
        1,
        "撤销后只剩未被撤销的那条直授"
    );
    assert_eq!(
        harness::authz_version_of(&app, holder).await,
        version_before + 1,
        "同 user_id 多条删除必须合并为一次授权版本递增"
    );

    // 新签发的令牌立即不含被撤销权限（解析侧按当前事实快照注入 claims）。
    let fresh_permissions = token_permissions(&harness::login_as(&app, "holder").await)
        .unwrap_or_else(|error| panic!("校验刷新 Token claims 失败: {error}"));
    for permission in ["access.grants.read", "access.groups.read"] {
        assert!(
            !fresh_permissions.iter().any(|p| p == permission),
            "刷新后的令牌不得再含被撤销的 {permission}，实际 {fresh_permissions:?}"
        );
    }
    assert!(
        fresh_permissions.iter().any(|p| p == "access.grants.write"),
        "未被撤销的权限必须保留，实际 {fresh_permissions:?}"
    );

    // 旧令牌失效：批量撤销只递增版本 + 追加 Outbox，不经 revoke_by_subject 即时收敛。
    // 水位线由 Outbox Worker 异步发布，本夹具不启动 Worker；校验器快速路径的缓存键
    // 刚被旧版本回填（TTL 5s，ADR 承认的最坏陈旧窗口），需等它过期后回查主库才能观测
    // 到 AuthorizationStale——与 `admin_can_remove_themselves_when_another_admin_remains`
    // 的等待同款；等待期间不得发探针，否则会把旧版本重新回填进缓存。
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    assert!(
        harness::member_token_is_stale(&app, &holder_token).await,
        "批量撤销后旧 Token 必须失效（授权版本水位线）"
    );

    // 幂等：重复撤销同一批 → 全部跳过、不再删行、不再动版本。
    let version_after = harness::authz_version_of(&app, holder).await;
    let response = harness::batch_revoke_permissions(&app, &admin, &revoke_items)
        .await
        .unwrap_or_else(|error| panic!("幂等重放批量撤销失败: {error}"));
    assert_eq!(response.code, 0, "幂等重放必须成功: {}", response.message);
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("幂等重放响应缺少 data: {response:?}"));
    assert_eq!(
        data["succeeded"].as_u64(),
        Some(0),
        "本就没有的条目不得删除: {data}"
    );
    assert_eq!(
        data["skipped"].as_u64(),
        Some(2),
        "本就没有的条目必须全部幂等跳过: {data}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, holder).await,
        1,
        "幂等重放不得再删行"
    );
    assert_eq!(
        harness::authz_version_of(&app, holder).await,
        version_after,
        "幂等撤销不得再次递增授权版本"
    );
}

/// G2 闸门的批量形态：批量里混入管理员等价权限 → 非全权组成员**整批**被拒（403）。
///
/// 事务原子语义（AUTHZ_GRANTS.md 批量授予）：任一失败整体回滚，不得产生部分写入。
/// 与单条闸门用例（`a_non_admin_cannot_grant_an_admin_equivalent_permission`）的差别
/// 是「批内无罪的普通条目」也必须一起回滚，且错误信息带 `items[N]` 索引定位失败位置。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_batch_grant_containing_an_admin_equivalent_permission_is_rejected_whole() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    // 攻击者：只直授 access.grants.write，完全不在全权组内（同单条闸门用例）。
    let outsider = harness::grant_only(&app, &admin, "outsider", "access.grants.write").await;
    let target = harness::register_with_code(&app, "target", "target@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 target 失败: {error}"));
    let version_before = harness::authz_version_of(&app, target).await;

    // 批内混入一条管理员等价权限（G2 清单：account.users.reset_credentials）。
    let items = [
        harness::grant_item(target, "access.grants.read"),
        harness::grant_item(target, "account.users.reset_credentials"),
    ];
    let (status, message) = harness::batch_grant_permissions_outcome(&app, &outsider, &items).await;
    assert_eq!(
        status, 403,
        "非全权组成员批量授予管理员等价权限必须 403 PermissionDenied，实际 {status}: {message}"
    );
    assert!(
        message.contains("系统管理员"),
        "拒绝信息必须点名只有系统管理员可以授予，实际 {message}"
    );
    assert!(
        message.contains("items[1]"),
        "拒绝信息必须带条目索引定位失败位置，实际 {message}"
    );

    // 无部分写入：被拒整批回滚，target 不得有任何直授行、授权版本不得动。
    assert_eq!(
        harness::grant_rows_of_user(&app, target).await,
        0,
        "被拒的批量不得落任何直授行（批内无罪的普通条目一并回滚）"
    );
    assert_eq!(
        harness::authz_version_of(&app, target).await,
        version_before,
        "被拒的批量不得递增授权版本"
    );

    // 反向对照：同一操作者授予纯普通权限的批量必须成功——拒绝面过宽同样是故障。
    let items = [
        harness::grant_item(target, "access.grants.read"),
        harness::grant_item(target, "feishu.datasource.read"),
    ];
    let response = harness::batch_grant_permissions(&app, &outsider, &items)
        .await
        .unwrap_or_else(|error| panic!("批量授予普通权限失败: {error}"));
    assert_eq!(
        response.code, 0,
        "同一操作者批量授予普通权限必须成功: {}",
        response.message
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target).await,
        2,
        "普通批量必须真的落库"
    );
}

/// 过期语义的端到端覆盖：短命直授（expires_at = now+2s）先生效、到期后从新令牌
/// claims 消失、行按审计保留并带 expired 派生标记。
///
/// 解析侧（`AuthzGrantResolver` → `list_by_user_in_tx`）把过期过滤下推到 SQL
/// （`expires_at IS NULL OR expires_at > now`）：过期只让权限在读取侧失效，行不删，
/// 为「重新授予走续期」保留审计痕迹。sleep 是集成测试的既定模式（同
/// `admin_can_remove_themselves_when_another_admin_remains`）；到期判定是 wall-clock
/// 秒级，睡过到期点后刷新令牌即可观测。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn an_expired_grant_disappears_from_refreshed_tokens_but_stays_for_audit() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let holder = harness::register_with_code(&app, "holder", "holder@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 holder 失败: {error}"));

    // 授予一条永久 + 一条短命（now+2s）权限；短命条目的过期时间由批量条目显式携带。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| panic!("系统时间早于 Unix epoch: {error}"))
        .as_secs() as i64;
    let expires_at = now + 2;
    let items = [
        harness::grant_item(holder, "access.grants.read"),
        harness::grant_item_with_expiry(holder, "access.groups.read", expires_at),
    ];
    let response = harness::batch_grant_permissions(&app, &admin, &items)
        .await
        .unwrap_or_else(|error| panic!("批量授予（含短命条目）失败: {error}"));
    assert_eq!(
        response.code, 0,
        "带 expires_at 的批量授予必须成功: {}",
        response.message
    );

    // 未过期时立即签发：两条都在 claims（解析侧 SQL 过滤只挡已过期的行）。
    let permissions = token_permissions(&harness::login_as(&app, "holder").await)
        .unwrap_or_else(|error| panic!("校验未到期 Token claims 失败: {error}"));
    assert!(
        permissions.iter().any(|p| p == "access.groups.read"),
        "未到期的短命权限必须仍在新令牌 claims 里，实际 {permissions:?}"
    );
    assert!(
        permissions.iter().any(|p| p == "access.grants.read"),
        "永久权限必须在新令牌 claims 里，实际 {permissions:?}"
    );

    // 审计视图：短命行此刻未过期、expires_at 原样回显。
    let short_lived = harness::grant_view_of(&app, &admin, holder, "access.groups.read")
        .await
        .unwrap_or_else(|| panic!("审计视图缺少 access.groups.read 条目"));
    assert_eq!(
        short_lived["expires_at"].as_i64(),
        Some(expires_at),
        "审计视图必须回显原始 expires_at: {short_lived}"
    );
    assert_eq!(
        short_lived["expired"].as_bool(),
        Some(false),
        "未到期的行不得标记已过期: {short_lived}"
    );

    // 睡过到期点（now+2s，等 3 秒保证 now > expires_at 的判定稳定成立）。
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // 刷新令牌：过期权限不再出现在 claims，永久那条不受影响。
    let refreshed = token_permissions(&harness::login_as(&app, "holder").await)
        .unwrap_or_else(|error| panic!("校验过期后刷新 Token claims 失败: {error}"));
    assert!(
        !refreshed.iter().any(|p| p == "access.groups.read"),
        "过期后刷新令牌不得再含该权限，实际 {refreshed:?}"
    );
    assert!(
        refreshed.iter().any(|p| p == "access.grants.read"),
        "永久权限不受过期影响，实际 {refreshed:?}"
    );

    // 审计视图：过期行仍在（行保留做审计）、expired 标记翻转、行数不变。
    let short_lived = harness::grant_view_of(&app, &admin, holder, "access.groups.read")
        .await
        .unwrap_or_else(|| panic!("审计视图缺少 access.groups.read 条目"));
    assert_eq!(
        short_lived["expires_at"].as_i64(),
        Some(expires_at),
        "过期行必须保留原始 expires_at: {short_lived}"
    );
    assert_eq!(
        short_lived["expired"].as_bool(),
        Some(true),
        "审计视图必须把过期行标记为已过期: {short_lived}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, holder).await,
        2,
        "过期只让权限在读取侧失效，直授行不得删除"
    );
}

/// 续期语义的端到端覆盖：短命直授（expires_at = now+2s）过期后，对同一
/// (user_id, permission) 重新授予（不带 expires_at = 永久）必须走续期分支——
/// succeeded=1 而非幂等跳过、expires_at 被重置为 NULL（审计视图 expired=false、
/// expires_at 空）、授权版本恰好递增一次、新令牌 claims 含该权限且后续刷新仍含
/// （永久 = 不再随时间失效）。
///
/// 单条 `grant_permission` 与批量 `batch_grant_permissions` 各有一条续期路径
/// （`renew_in_tx` 原地 UPDATE 保留审计痕迹，唯一键不允许插第二行），两个入口都测：
/// 本次 minor 对抗指出的缺口正是单条路径没有端到端断言。sleep 是集成测试的既定
/// 模式（同 `an_expired_grant_disappears_from_refreshed_tokens_but_stays_for_audit`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_renewed_grant_resets_expiry_and_increments_version_once() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let holder = harness::register_with_code(&app, "holder_renew", "holder_renew@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 holder_renew 失败: {error}"));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| panic!("系统时间早于 Unix epoch: {error}"))
        .as_secs() as i64;
    let expires_at = now + 2;

    // ---- 单条 grant_permission 的续期路径 ----
    let version_before = harness::authz_version_of(&app, holder).await;
    let granted = harness::grant_permission_response(
        &app,
        &admin,
        holder,
        "access.groups.read",
        Some(expires_at),
    )
    .await
    .unwrap_or_else(|error| panic!("单条授予短命权限失败: {error}"));
    assert_eq!(
        granted.data.as_ref().and_then(|d| d["changed"].as_bool()),
        Some(true),
        "首次授予必须 changed=true: {:?}",
        granted.data
    );
    assert_eq!(
        harness::authz_version_of(&app, holder).await,
        version_before + 1,
        "首次授予必须恰好递增一次授权版本"
    );

    // 睡过到期点（now+2s，等 3 秒保证 now > expires_at 的判定稳定成立）。
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // 重新授予同一 (user_id, permission)，不带 expires_at = 永久续期。
    let version_before = harness::authz_version_of(&app, holder).await;
    let renewed =
        harness::grant_permission_response(&app, &admin, holder, "access.groups.read", None)
            .await
            .unwrap_or_else(|error| panic!("单条续期失败: {error}"));
    assert_eq!(
        renewed.data.as_ref().and_then(|d| d["changed"].as_bool()),
        Some(true),
        "过期行重授必须走续期分支（changed=true 而非幂等跳过）: {:?}",
        renewed.data
    );
    assert_eq!(
        harness::authz_version_of(&app, holder).await,
        version_before + 1,
        "续期必须恰好递增一次授权版本（不得重复递增）"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, holder).await,
        1,
        "续期是原地 UPDATE，直授行数不得增加"
    );

    // 审计视图：expires_at 被重置为空（永久）、expired=false。
    let renewed_view = harness::grant_view_of(&app, &admin, holder, "access.groups.read")
        .await
        .unwrap_or_else(|| panic!("审计视图缺少 access.groups.read 条目"));
    assert!(
        renewed_view["expires_at"].is_null(),
        "续期为永久必须清空 expires_at: {renewed_view}"
    );
    assert_eq!(
        renewed_view["expired"].as_bool(),
        Some(false),
        "续期后不得标记为已过期: {renewed_view}"
    );

    // claims：新令牌含该权限，且后续刷新仍含（永久 = 不再随时间失效）。
    let permissions = token_permissions(&harness::login_as(&app, "holder_renew").await)
        .unwrap_or_else(|error| panic!("校验续期后 Token claims 失败: {error}"));
    assert!(
        permissions.iter().any(|p| p == "access.groups.read"),
        "续期后新令牌 claims 必须含该权限，实际 {permissions:?}"
    );
    let refreshed = token_permissions(&harness::login_as(&app, "holder_renew").await)
        .unwrap_or_else(|error| panic!("校验再次刷新 Token claims 失败: {error}"));
    assert!(
        refreshed.iter().any(|p| p == "access.groups.read"),
        "续期为永久后刷新令牌必须仍含该权限，实际 {refreshed:?}"
    );

    // ---- 批量 batch_grant_permissions 的续期路径（同款语义的另一入口） ----
    // 前面的 sleep 已推进 wall-clock：批量条目的 expires_at 必须按此刻重新计算
    // （handler 校验「过期时间必须大于当前时间」用的是它自己的时钟）。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| panic!("系统时间早于 Unix epoch: {error}"))
        .as_secs() as i64;
    let expires_at = now + 2;
    let version_before = harness::authz_version_of(&app, holder).await;
    let short_lived = harness::batch_grant_permissions(
        &app,
        &admin,
        &[harness::grant_item_with_expiry(
            holder,
            "feishu.datasource.read",
            expires_at,
        )],
    )
    .await
    .unwrap_or_else(|error| panic!("批量授予短命权限失败: {error}"));
    assert_eq!(
        short_lived
            .data
            .as_ref()
            .and_then(|d| d["succeeded"].as_u64()),
        Some(1),
        "批量首次授予必须 succeeded=1: {:?}",
        short_lived.data
    );
    assert_eq!(
        harness::authz_version_of(&app, holder).await,
        version_before + 1,
        "批量首次授予必须恰好递增一次授权版本"
    );

    // 睡过到期点后批量重授（永久）。
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let version_before = harness::authz_version_of(&app, holder).await;
    let renewed = harness::batch_grant_permissions(
        &app,
        &admin,
        &[harness::grant_item(holder, "feishu.datasource.read")],
    )
    .await
    .unwrap_or_else(|error| panic!("批量续期失败: {error}"));
    assert_eq!(
        renewed.data.as_ref().and_then(|d| d["succeeded"].as_u64()),
        Some(1),
        "过期行批量重授必须走续期分支（succeeded=1 而非 skipped）: {:?}",
        renewed.data
    );
    assert_eq!(
        renewed.data.as_ref().and_then(|d| d["skipped"].as_u64()),
        Some(0),
        "续期不得计为幂等跳过: {:?}",
        renewed.data
    );
    assert_eq!(
        harness::authz_version_of(&app, holder).await,
        version_before + 1,
        "批量续期必须恰好递增一次授权版本"
    );
    let renewed_view = harness::grant_view_of(&app, &admin, holder, "feishu.datasource.read")
        .await
        .unwrap_or_else(|| panic!("审计视图缺少 feishu.datasource.read 条目"));
    assert!(
        renewed_view["expires_at"].is_null(),
        "批量续期为永久必须清空 expires_at: {renewed_view}"
    );
    assert_eq!(
        renewed_view["expired"].as_bool(),
        Some(false),
        "批量续期后不得标记为已过期: {renewed_view}"
    );
    let refreshed = token_permissions(&harness::login_as(&app, "holder_renew").await)
        .unwrap_or_else(|error| panic!("校验批量续期后 Token claims 失败: {error}"));
    assert!(
        refreshed.iter().any(|p| p == "feishu.datasource.read"),
        "批量续期为永久后刷新令牌必须仍含该权限，实际 {refreshed:?}"
    );
}

/// 批量条数上限的端到端覆盖：>100 条必须 400（ParamInvalid）、101 条不落库；
/// 100 条边界必须仍可受理（拒绝面过宽同样是故障）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn batch_actions_over_the_item_limit_are_rejected_with_400() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let target = harness::register_with_code(&app, "target", "target@example.com")
        .await
        .unwrap_or_else(|error| panic!("注册 target 失败: {error}"));

    // 批量授予：101 条 → 400，且不得落任何行。
    let mut items = Vec::new();
    for _ in 0..101 {
        items.push(harness::grant_item(target, "access.grants.read"));
    }
    let (status, message) = harness::batch_grant_permissions_outcome(&app, &admin, &items).await;
    assert_eq!(
        status, 400,
        "超过 100 条上限必须 ParamInvalid→400，实际 {status}: {message}"
    );
    assert!(
        message.contains("100"),
        "拒绝信息必须点名上限条数，实际 {message}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target).await,
        0,
        "被拒的批量不得落任何直授行"
    );

    // 边界反向对照：恰好 100 条必须仍可受理（批内同 user×perm 的重复条目幂等跳过）。
    let mut items = Vec::new();
    for _ in 0..100 {
        items.push(harness::grant_item(target, "access.grants.read"));
    }
    let response = harness::batch_grant_permissions(&app, &admin, &items)
        .await
        .unwrap_or_else(|error| panic!("恰好 100 条的批量必须成功: {error}"));
    assert_eq!(
        response.code, 0,
        "恰好 100 条必须成功: {}",
        response.message
    );
    let data = response
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("恰好 100 条响应缺少 data: {response:?}"));
    assert_eq!(
        data["succeeded"].as_u64(),
        Some(1),
        "批内重复条目只写一次: {data}"
    );
    assert_eq!(
        data["skipped"].as_u64(),
        Some(99),
        "批内重复条目按幂等跳过: {data}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target).await,
        1,
        "同 (user_id, permission) 只落一行"
    );

    // 批量撤销同一条上限：101 条 → 400，不得删任何行。
    let mut items = Vec::new();
    for _ in 0..101 {
        items.push(harness::revoke_item(target, "access.grants.read"));
    }
    let (status, message) = harness::batch_revoke_permissions_outcome(&app, &admin, &items).await;
    assert_eq!(
        status, 400,
        "批量撤销超过 100 条上限必须 ParamInvalid→400，实际 {status}: {message}"
    );
    assert!(
        message.contains("100"),
        "拒绝信息必须点名上限条数，实际 {message}"
    );
    assert_eq!(
        harness::grant_rows_of_user(&app, target).await,
        1,
        "被拒的批量撤销不得删任何行"
    );
}
