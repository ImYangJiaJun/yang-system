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
        build_application, connect_test_database, connect_test_redis, dispatch, dispatch_with_path,
        login, reset_database, reset_redis, PASSWORD, SYSTEM_ADMIN_GROUP_KEY,
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

    /// 给已存在的账号补一条直授权限（受 Step-up 保护），不重新签发令牌。
    ///
    /// [`grant_only`] 只覆盖「恰好一条权限」的夹具；锁序用例需要操作者既持
    /// `access.groups.write`（否则调用不了组条目 Action），又持一条它准备写进组的
    /// 权限（否则会撞上 §8.1 的子集校验），因此需要这条补充授权的路径。
    pub async fn grant_permission(app: &BuiltApp, admin: &Admin, user_id: i64, permission: &str) {
        step_up_dispatch(
            app,
            "access.grants",
            "grant_permission",
            json!({ "user_id": user_id, "permission": permission }),
            admin,
        )
        .await
        .unwrap_or_else(|error| panic!("授予账号 {user_id} 权限 {permission} 失败: {error}"));
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

    /// 组成员写 Action 的请求体（proof 的指纹绑定它，故取 proof 与真调用必须同源）。
    fn member_body(group_id: i64, user_id: i64) -> Value {
        json!({ "group_id": group_id, "user_id": user_id })
    }

    /// 驱动一个组成员写 Action：**完整**走 Step-up（无 proof 触发 challenge →
    /// 密码重认证 → 带 proof 重试）。
    ///
    /// 成员加/移出与条目、组生命周期同属授权事实变更，都已在 `step_up_targets()` 登记，
    /// 因此这里不能再用裸认证请求——若守卫真的缺失，[`step_up_dispatch`] 会把「无 proof
    /// 也成功」折算成 `ConfigError`（500），用例立刻变红。
    async fn member_mutation(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        user_id: i64,
    ) -> Result<ApiResponse, BaseError> {
        step_up_dispatch(
            app,
            "access.groups",
            action,
            member_body(group_id, user_id),
            operator,
        )
        .await
    }

    /// 预先取得一次组成员写 Action 的 Step-up proof（并发用例专用）。
    ///
    /// challenge 与请求指纹绑定（body + 路径 + 查询），因此取 proof 与随后携带它的
    /// 调用必须用同一个 body——两者都经 [`member_body`] 构造。
    pub async fn member_mutation_proof(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        user_id: i64,
    ) -> String {
        step_up_proof_for_body(
            app,
            operator,
            "access.groups",
            action,
            member_body(group_id, user_id),
        )
        .await
    }

    /// 携带**已取得的** proof 驱动组成员写 Action，折算为（状态码, 消息）。
    ///
    /// 并发用例必须这样调用：把重认证移出临界窗口后，Barrier 对齐的才是真正的
    /// 「事务内读判据 → 写事实」那一段。
    pub async fn member_mutation_with_proof(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        user_id: i64,
        proof: &str,
    ) -> (u16, String) {
        group_action_with_proof(app, operator, action, member_body(group_id, user_id), proof).await
    }

    /// 直接驱动一个组管理 Action 而**不**携带 Step-up proof，返回原始结果。
    ///
    /// 与 [`step_up_dispatch`] 的差别是本函数刻意不做自愈重试，因此「守卫缺失」
    /// 会原样表现为成功——这正是守卫测试要钉死的那件事。
    pub async fn group_action_without_step_up(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        body: Value,
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", operator.token);
        dispatch(
            app,
            "access.groups",
            action,
            body,
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
        step_up_dispatch_with_path(app, module, action, body, admin, &[]).await
    }

    /// 同 [`step_up_dispatch`]，额外携带路径参数（如 `/api/v1/users/{id}/disable`）。
    async fn step_up_dispatch_with_path(
        app: &BuiltApp,
        module: &str,
        action: &str,
        body: Value,
        admin: &Admin,
        path_params: &[(&str, &str)],
    ) -> Result<ApiResponse, BaseError> {
        let authorization = format!("Bearer {}", admin.token);
        match dispatch_with_path(
            app,
            module,
            action,
            body.clone(),
            &[("authorization", authorization.as_str())],
            PEER_PORT,
            path_params,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => {
                let proof = complete_step_up(app, admin, challenge).await?;
                dispatch_with_path(
                    app,
                    module,
                    action,
                    body,
                    &[
                        ("authorization", authorization.as_str()),
                        (STEP_UP_PROOF_HEADER, proof.as_str()),
                    ],
                    PEER_PORT,
                    path_params,
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

    /// 用操作者本人的密码完成一次 Step-up 重认证，返回一次性 proof。
    ///
    /// 与 [`step_up_dispatch_with_path`] 内部那两跳等价，单独抽出来是为了让并发用例
    /// 能把重认证**移出**临界窗口：先取好 proof，再用 Barrier 同时发起携 proof 的那一次调用。
    async fn complete_step_up(
        app: &BuiltApp,
        admin: &Admin,
        challenge: yang_base::action::StepUpChallenge,
    ) -> Result<String, BaseError> {
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
        completed
            .data
            .as_ref()
            .and_then(|data| data["proof"].as_str())
            .map(str::to_string)
            .ok_or_else(|| BaseError::ConfigError("Step-up 完成响应缺少 proof".to_string()))
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

    /// 组条目写 Action 的请求体（proof 的指纹绑定它，故取 proof 与真调用必须同源）。
    fn item_body(group_id: i64, permission: &str) -> Value {
        json!({ "group_id": group_id, "permission": permission })
    }

    /// 驱动一个组条目写 Action：**完整**走 Step-up（与成员、组生命周期同例）。
    ///
    /// 条目加/移除直接改变授权事实，已在 `step_up_targets()` 登记；若守卫缺失，
    /// [`step_up_dispatch`] 会把「无 proof 也成功」折算成 `ConfigError`（500）。
    async fn item_mutation(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        permission: &str,
    ) -> Result<ApiResponse, BaseError> {
        step_up_dispatch(
            app,
            "access.groups",
            action,
            item_body(group_id, permission),
            operator,
        )
        .await
    }

    /// 预先取得一次组条目写 Action 的 Step-up proof（并发用例专用）。
    pub async fn item_mutation_proof(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        permission: &str,
    ) -> String {
        step_up_proof_for_body(
            app,
            operator,
            "access.groups",
            action,
            item_body(group_id, permission),
        )
        .await
    }

    /// 携带**已取得的** proof 驱动组条目写 Action，折算为（状态码, 消息）。
    pub async fn item_mutation_with_proof(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        group_id: i64,
        permission: &str,
        proof: &str,
    ) -> (u16, String) {
        group_action_with_proof(
            app,
            operator,
            action,
            item_body(group_id, permission),
            proof,
        )
        .await
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
    /// `admin_disable_user` 要求 `account.users.manage` 且受 Step-up 保护，因此操作者
    /// 身份由调用方给出——最后管理员用例刻意让一个**非系统管理员**来当操作者，
    /// 否则该 Action 既有的「不能停用自己」前置会先把用例挡在守卫之前。
    ///
    /// 目标用户是**路径参数**（`/api/v1/users/{id}/disable`），必须经
    /// `step_up_dispatch_with_path` 传入 `request.path_params`。
    pub async fn admin_disable_status(
        app: &BuiltApp,
        operator: &Admin,
        target_user_id: i64,
    ) -> u16 {
        let target = target_user_id.to_string();
        match step_up_dispatch_with_path(
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

    /// 尝试自助停用当前账号并折算为 HTTP 状态码。
    pub async fn disable_self_status(app: &BuiltApp, actor: &Admin) -> u16 {
        match step_up_dispatch(app, "account.user", "disable_self", json!({}), actor).await {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
    }

    /// 尝试匿名化删除当前账号并折算为 HTTP 状态码。
    pub async fn delete_account_status(app: &BuiltApp, actor: &Admin) -> u16 {
        match step_up_dispatch(
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

    /// 预先取得一次 Step-up 重认证的 proof。
    ///
    /// 并发用例必须把重认证**移出**临界窗口：`step_up_dispatch` 内部是「无 proof
    /// 触发 challenge → 完成重认证 → 带 proof 重试」三跳，两个任务在这三跳上的
    /// 抖动（含 argon2 校验耗时）会远超真正要观测的事务窗口，用例会退化成
    /// 「串行两次调用」，根本压不到临界点。因此这里只负责取 proof，由用例自己在
    /// 拿到两份 proof 之后用 Barrier 同时发起携带 proof 的那一次调用。
    ///
    /// proof 与 Action 绑定（challenge 由该 Action 的无 proof 调用签发），
    /// 所以 `action` 必须与随后携带它的那次调用完全一致。
    pub async fn step_up_proof_for(app: &BuiltApp, admin: &Admin, action: &str) -> String {
        step_up_proof_for_body(app, admin, "account.user", action, json!({})).await
    }

    /// 同 [`step_up_proof_for`]，但可指定 Module 与请求体——challenge 与请求指纹
    /// （body + 路径 + 查询）绑定，故 `body` 必须与随后携带该 proof 的调用逐字节同源。
    ///
    /// 无 proof 的那一次若直接成功，说明 Step-up 守卫根本没挂上：那是安全回归，
    /// 必须让用例失败而不是放行，因此这里 panic。
    pub async fn step_up_proof_for_body(
        app: &BuiltApp,
        admin: &Admin,
        module: &str,
        action: &str,
        body: Value,
    ) -> String {
        let authorization = format!("Bearer {}", admin.token);
        let challenge = match dispatch(
            app,
            module,
            action,
            body,
            &[("authorization", authorization.as_str())],
            PEER_PORT,
        )
        .await
        {
            Err(BaseError::StepUpRequired(challenge)) => challenge,
            Ok(response) => panic!(
                "{module}.{action} 未受 Step-up 保护：无 proof 也成功了（{}）",
                response.message
            ),
            Err(other) => panic!("{module}.{action} 触发 Step-up 失败: {other}"),
        };
        complete_step_up(app, admin, challenge)
            .await
            .unwrap_or_else(|error| panic!("Step-up 完成失败: {error}"))
    }

    /// 携带**已取得的** proof 驱动一个组管理 Action，折算为（状态码, 消息）。
    ///
    /// proof 是不可重放的：每个并发任务都必须持有自己那一份。
    pub async fn group_action_with_proof(
        app: &BuiltApp,
        operator: &Admin,
        action: &str,
        body: Value,
        proof: &str,
    ) -> (u16, String) {
        let authorization = format!("Bearer {}", operator.token);
        match dispatch(
            app,
            "access.groups",
            action,
            body,
            &[
                ("authorization", authorization.as_str()),
                (STEP_UP_PROOF_HEADER, proof),
            ],
            PEER_PORT,
        )
        .await
        {
            Ok(response) => (200, response.message),
            Err(error) => (http_status(&error), error.to_string()),
        }
    }

    /// 携带**已取得的** proof 自助停用，折算为 HTTP 状态码。
    ///
    /// 与 `disable_self_status` 的区别只在于 proof 由调用方先行取得，从而让
    /// 真正的临界区（开启事务 → 读管理员计数 → 写停用）能被 Barrier 精确对齐。
    pub async fn disable_self_with_proof(app: &BuiltApp, actor: &Admin, proof: &str) -> u16 {
        let authorization = format!("Bearer {}", actor.token);
        match dispatch(
            app,
            "account.user",
            "disable_self",
            json!({}),
            &[
                ("authorization", authorization.as_str()),
                (STEP_UP_PROOF_HEADER, proof),
            ],
            PEER_PORT,
        )
        .await
        {
            Ok(_) => 200,
            Err(error) => http_status(&error),
        }
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
    assert_eq!(disable_status, 400, "最后一名系统管理员不可被停用");

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

/// 有两名管理员时，移除其一必须成功（守卫不能过度收紧）。
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

/// spec §8.3：账号删除后不得残留授权事实。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn deleting_an_account_leaves_no_orphan_authorization_rows() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let user = harness::grant_only(&app, &admin, "victim", "demo.notes.read").await;
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
/// 两个管理员各自自助停用，用真库两条独立连接 + Barrier 让两次「读管理员计数 →
/// 写停用」真正撞在一起。若计数是在事务外经连接池无锁读出的（缺陷形态），两边都会
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

        // 重认证先行完成，临界区只留「事务内读计数 + 写停用」。
        let first_proof = harness::step_up_proof_for(&app, &first, "disable_self").await;
        let second_proof = harness::step_up_proof_for(&app, &second, "disable_self").await;

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (actor, proof) in [(first.clone(), first_proof), (second.clone(), second_proof)] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::disable_self_with_proof(&app, &actor, &proof).await
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
        // 反向钉住「不得双双放行」：清零必然意味着两次都成功，所以这一条与上面
        // 那条等价，但它把「拒绝」这件事也钉进事实里，避免未来把守卫改成静默忽略。
        assert!(
            statuses.iter().filter(|status| **status == 200).count() <= 1,
            "第 {round} 轮两个并发停用不得都成功，实际响应 {statuses:?}"
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

        // 重认证移出临界窗口：两个任务各持一份一次性 proof。若让每个任务在 Barrier 之后
        // 自己走「challenge → argon2 → 重试」三跳，两次真正插入会被 argon2 的耗时错开，
        // 用例会退化成串行两次调用，压不到唯一键冲突那条路径。
        let mut proofs = Vec::new();
        for _ in 0..2 {
            proofs.push(
                harness::member_mutation_proof(&app, &admin, "add_group_member", group_id, target)
                    .await,
            );
        }

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for proof in proofs {
            let app = app.clone();
            let admin = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                // 用 outcome 而非 status：失败时要能读出真正的错误文案，
                // 否则 5xx/404 只留下一串状态码，定位不到是哪条错误被折算出来的。
                harness::member_mutation_with_proof(
                    &app,
                    &admin,
                    "add_group_member",
                    group_id,
                    target,
                    &proof,
                )
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
// R1（收尾回合）：成员关系读锁定化——成员变更必须以**成员行**为串行化点。
// ---------------------------------------------------------------------------

/// 用 Barrier 同时驱动两条组成员写请求（各自携带先行取得的一次性 proof）。
///
/// 两个任务的操作者**分别给出**：并发用例必须能用两个不同的账号发起请求，否则两次
/// 请求会因为锁批里共有的操作者行而被串行化，从而压不出「两个并发请求各自读到同一份
/// 陈旧快照」这个窗口。
///
/// 把重认证移出临界窗口是既有并发用例的统一做法：Barrier 对齐的必须是
/// 「开事务 → 读判据 → 写事实」那一段，而不是三跳 Step-up 的耗时抖动。
async fn concurrent_member_mutations(
    app: &BuiltApp,
    group_id: i64,
    left: (harness::Admin, &'static str, i64, String),
    right: (harness::Admin, &'static str, i64, String),
) -> Vec<u16> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for (operator, action, user_id, proof) in [left, right] {
        let app = app.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            harness::member_mutation_with_proof(&app, &operator, action, group_id, user_id, &proof)
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
/// 真库 + 两条独立连接 + Barrier：两个任务先各自取好一次性 proof，Barrier 对齐的因此
/// 是「开事务 → 读成员 → 判据 → 删成员行」那一段。两个场景各跑三轮，避免调度偶然。
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

        let remove_other = harness::member_mutation_proof(
            &app,
            &first,
            "remove_group_member",
            group_id,
            second.user_id,
        )
        .await;
        let remove_self = harness::member_mutation_proof(
            &app,
            &first,
            "remove_group_member",
            group_id,
            first.user_id,
        )
        .await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (
                first.clone(),
                "remove_group_member",
                second.user_id,
                remove_other,
            ),
            (
                first.clone(),
                "remove_group_member",
                first.user_id,
                remove_self,
            ),
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

        let first_removes_second = harness::member_mutation_proof(
            &app,
            &first,
            "remove_group_member",
            group_id,
            second.user_id,
        )
        .await;
        let second_removes_first = harness::member_mutation_proof(
            &app,
            &second,
            "remove_group_member",
            group_id,
            first.user_id,
        )
        .await;

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (operator, target, proof) in [
            (first.clone(), second.user_id, first_removes_second),
            (second.clone(), first.user_id, second_removes_first),
        ] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_with_proof(
                    &app,
                    &operator,
                    "remove_group_member",
                    group_id,
                    target,
                    &proof,
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

        let left_proof = harness::member_mutation_proof(
            &app,
            &left_operator,
            "add_group_member",
            group_id,
            left,
        )
        .await;
        let right_proof = harness::member_mutation_proof(
            &app,
            &right_operator,
            "add_group_member",
            group_id,
            right,
        )
        .await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (left_operator, "add_group_member", left, left_proof),
            (right_operator, "add_group_member", right, right_proof),
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

        let left_proof = harness::member_mutation_proof(
            &app,
            &left_operator,
            "add_group_member",
            group_id,
            left,
        )
        .await;
        let right_proof = harness::member_mutation_proof(
            &app,
            &right_operator,
            "add_group_member",
            group_id,
            right,
        )
        .await;

        let statuses = concurrent_member_mutations(
            &app,
            group_id,
            (left_operator, "add_group_member", left, left_proof),
            (right_operator, "add_group_member", right, right_proof),
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

        let remove_proof = harness::member_mutation_proof(
            &app,
            &admin,
            "remove_group_member",
            admin_group,
            third.user_id,
        )
        .await;
        let item_proof = harness::item_mutation_proof(
            &app,
            &admin,
            "add_group_item",
            shared,
            "account.users.read",
        )
        .await;

        let removed = third.user_id;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = admin.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::member_mutation_with_proof(
                    &app,
                    &operator,
                    "remove_group_member",
                    admin_group,
                    removed,
                    &remove_proof,
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
                harness::item_mutation_with_proof(
                    &app,
                    &operator,
                    "add_group_item",
                    shared,
                    "account.users.read",
                    &item_proof,
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

        // 两条路径都是授权事实变更、都已在 Step-up 登记内，因此都要重认证。取 proof
        // 的那一次调用只留在 Step-up 守卫里（连 Action 都没进），不会动上面刚断言的
        // 前置事实；把重认证**移出**临界窗口后，Barrier 对齐的才是「各自第一条普通读
        // 建立快照」那一刻——这正是缺陷暴露的窗口。
        let item_proof =
            harness::item_mutation_proof(&app, &operator, "add_group_item", group_id, ESCALATED)
                .await;
        let member_proof = harness::member_mutation_proof(
            &app,
            &operator,
            "add_group_member",
            group_id,
            operator.user_id,
        )
        .await;

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        {
            let app = app.clone();
            let operator = operator.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_with_proof(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    ESCALATED,
                    &item_proof,
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
                harness::member_mutation_with_proof(
                    &app,
                    &operator,
                    "add_group_member",
                    group_id,
                    operator.user_id,
                    &member_proof,
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
        assert!(
            statuses.iter().filter(|status| **status == 200).count() <= 1,
            "第 {round} 轮两条路径不可能都成功：串行执行时第二个请求必被 §8.1 子集校验\
             拒绝（403），实际 {statuses:?}"
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

        // 重认证必须移出临界窗口，否则 argon2 的耗时会错开两次真正的事务，
        // 「不同 Action 的取锁顺序是否一致」就压不出来了。proof 与操作者主体绑定，
        // 因此两人各取各的。
        let item_proof = harness::item_mutation_proof(
            &app,
            &item_operator,
            "add_group_item",
            group_id,
            "access.grants.read",
        )
        .await;
        let member_proof = harness::member_mutation_proof(
            &app,
            &member_operator,
            "add_group_member",
            group_id,
            newcomer,
        )
        .await;

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
                harness::item_mutation_with_proof(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    "access.grants.read",
                    &item_proof,
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
                harness::member_mutation_with_proof(
                    &app,
                    &operator,
                    "add_group_member",
                    group_id,
                    newcomer,
                    &member_proof,
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

        // 重认证移出临界窗口（proof 与主体绑定，两人各取各的），Barrier 对齐的才是
        // 两次「锁操作者行 + 按升序锁成员行」的事务本身。
        let first_proof = harness::item_mutation_proof(
            &app,
            &first,
            "add_group_item",
            group_id,
            "access.grants.read",
        )
        .await;
        let second_proof = harness::item_mutation_proof(
            &app,
            &second,
            "add_group_item",
            group_id,
            "account.users.read",
        )
        .await;

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (operator, permission, proof) in [
            (first.clone(), "access.grants.read", first_proof),
            (second.clone(), "account.users.read", second_proof),
        ] {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                harness::item_mutation_with_proof(
                    &app,
                    &operator,
                    "add_group_item",
                    group_id,
                    permission,
                    &proof,
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
// R4：Step-up 全覆盖——未完成重认证的调用不得触达任何一个组管理 Action。
// ---------------------------------------------------------------------------

/// spec §9.2 与计划 Global Constraints：**全部**组管理 Action 都必须挂 Step-up。
///
/// 缺陷形态：`step_up_targets()` 只登记建/改/删组三个，条目与成员那四条 Action
/// （`add_group_item` / `remove_group_item` / `add_group_member` /
/// `remove_group_member`）没有守卫——持有一条被盗令牌的攻击者不用重认证就能改变
/// 授权事实。本用例逐个 Action 断言「不带 proof 的调用被拒」，是那条不变量在真实
/// 装配路径上的投影：守卫一旦漏挂，这里就会读到成功响应。
///
/// 与 `access.groups` 单测的分工：单测从冻结 Catalog 枚举写 Action 与登记清单比对
/// （防止漏登记），本用例证明登记真的被挂成了中间件、并且被折算成 428。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 MySQL/Redis"]
async fn group_mutations_without_step_up_are_rejected() {
    let app = harness::build_test_app().await;
    let admin = harness::bootstrap_admin(&app).await;
    let group_id = harness::create_group(&app, &admin, "stepup", "重认证覆盖组").await;
    let member = harness::register_with_code(&app, "stepup_member", "stepup_member@example.com")
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    let cases = [
        (
            "add_group_item",
            json!({ "group_id": group_id, "permission": "access.grants.read" }),
        ),
        (
            "remove_group_item",
            json!({ "group_id": group_id, "permission": "access.grants.read" }),
        ),
        (
            "add_group_member",
            json!({ "group_id": group_id, "user_id": member }),
        ),
        (
            "remove_group_member",
            json!({ "group_id": group_id, "user_id": member }),
        ),
    ];

    for (action, body) in cases {
        match harness::group_action_without_step_up(&app, &admin, action, body).await {
            Err(BaseError::StepUpRequired(_)) => {}
            Ok(response) => panic!(
                "{action} 未完成 Step-up 也执行了（code={}，{}）——组管理 Action 必须全部挂重认证",
                response.code, response.message
            ),
            Err(other) => panic!("{action} 必须返回 StepUpRequired（428 Step-up），实际 {other}"),
        }
    }

    // 拒绝必须是「没进业务」的拒绝：上面四次被拒的调用不得留下任何写结果。
    assert_eq!(
        harness::item_rows_of_group(&app, group_id).await,
        0,
        "被拒的条目写入不得落库"
    );
    assert!(
        !harness::is_member(&app, group_id, member).await,
        "被拒的成员写入不得落库"
    );
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
