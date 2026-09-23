//! 首账号引导（`SystemOwnerClaimer`）的真实库集成测试。
//!
//! 引导的全部行为都是数据库行为——竞争哨兵、幂等创建内置全权组、写入成员关系
//! 并使目标用户的授权版本失效；没有可单测的纯逻辑面。因此本文件的契约钉在
//! 真实 MySQL/Redis 上：先把「哨兵只被夺到一次」写成失败测试，再让实现追平。

mod common;

/// 测试夹具：真库/真 Redis 上的应用装配与直接的数据库观测。
///
/// Task 9 的并发引导对抗测试与本文件共用这套夹具；`register_with_code`
/// 走真实注册 Action（邮箱验证码 → 注册），是最贴近生产路径的驱动方式。
mod harness {
    use super::common::{take_registration_code, RegistrationEmailToolsExt};
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
    use yang_system::schema::{definitions, sync_with_database};

    /// 注册请求体里的固定口令（满足密码策略，与其它集成测试同值）。
    const PASSWORD: &str = "correct-horse-battery-staple";
    /// 内置全权组的固定标识（与受信 writer 的 `SYSTEM_ADMIN_GROUP_KEY` 同值）。
    const SYSTEM_ADMIN_GROUP_KEY: &str = "system_admin";
    /// 夹具固定的对端端口：注册限流按 IP 计数，阈值已放大到本用例不会触发。
    const PEER_PORT: u16 = 52_300;

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

    /// 注册与限流都要走完整凭据/风控语义，故与 account 侧集成测试同参数。
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
            "system-owner-bootstrap-active".to_string(),
            "system-owner-bootstrap-secret-32-bytes",
            Vec::new(),
            Algorithm::HS256,
            "yang-system-system-owner-bootstrap".to_string(),
            "yang-system-system-owner-bootstrap-api".to_string(),
            3600,
            2_592_000,
        )
        .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
    }

    fn step_up_manager() -> Arc<StepUpManager> {
        Arc::new(
            StepUpManager::new(
                "system-owner-bootstrap-step-up-secret-32-bytes",
                "yang-system-system-owner-bootstrap-step-up",
                "yang-system-system-owner-bootstrap-sensitive",
            )
            .unwrap_or_else(|error| panic!("测试 Step-up manager 应构建成功: {error}")),
        )
    }

    async fn connect_test_database() -> Database {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
            .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")
            .unwrap_or_else(|error| panic!("{error}"));
        let database = Database::connect_with_config(&url, database_config())
            .await
            .context("连接引导测试 MySQL 失败")
            .unwrap_or_else(|error| panic!("{error}"));
        let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await
            .context("读取引导测试数据库名失败")
            .unwrap_or_else(|error| panic!("{error}"));
        let name = name
            .context("引导测试连接没有选择数据库")
            .unwrap_or_else(|error| panic!("{error}"));
        if !name.ends_with("_test") {
            panic!("拒绝在非测试数据库 {name:?} 执行引导测试");
        }
        database
    }

    async fn connect_test_redis() -> RedisClient {
        let url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")
            .context("缺少 YANG_SYSTEM_TEST_REDIS_URL")
            .unwrap_or_else(|error| panic!("{error}"));
        if !url.trim_end_matches('/').ends_with("/15") {
            panic!("引导测试 Redis URL 必须使用独立 DB 15");
        }
        RedisClient::connect_with_config(&url, redis_config())
            .await
            .context("连接引导测试 Redis 失败")
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// 同步测试库 Schema 并装配完整应用；失败直接 panic（夹具不做错误恢复）。
    pub async fn build_test_app() -> BuiltApp {
        sync_with_database(
            connect_test_database().await,
            database_config(),
            security_settings(),
        )
        .await
        .unwrap_or_else(|error| panic!("同步引导测试 Schema 失败: {error}"));
        let redis = connect_test_redis().await;
        // 命名空间按部署唯一：验证码与授权版本缓存不跨用例串味。
        let deployment = format!(
            "system-owner-bootstrap-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_else(|error| panic!("系统时间早于 Unix epoch: {error}"))
                .as_nanos()
        );
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(connect_test_database().await)
                .cache(redis.clone())
                .with_registration_email(format!("email-{deployment}"))
                .extension(
                    AuthorizationVersionCache::new(redis, deployment)
                        .unwrap_or_else(|error| panic!("构建授权版本缓存失败: {error}")),
                )
                .extension(step_up_manager())
                .token(token_manager())
                .build()
                .unwrap_or_else(|error| panic!("构建引导测试 Tools 失败: {error}")),
        );
        build_app(tools, security_settings())
            .unwrap_or_else(|error| panic!("构建引导测试应用失败: {error}"))
            .runtime
    }

    /// 清空全部业务表，让每个用例从零账号、零哨兵、零组事实开始。
    ///
    /// 外键（RESTRICT）要求父表晚于子表清空，而这里不做静态排序：逐轮
    /// `DELETE`，被外键挡住的表下一轮再试。Schema 是有向无环的，因此每轮
    /// 至少清空一层，最终必然整轮无失败。
    pub async fn reset_business_tables(app: &BuiltApp) {
        let tables: Vec<String> = definitions(app)
            .unwrap_or_else(|error| panic!("读取应用表定义失败: {error}"))
            .iter()
            .map(|definition| definition.name().to_string())
            .collect();
        let pool = app
            .tools()
            .mysql()
            .unwrap_or_else(|error| panic!("引导测试应用必须配置 MySQL: {error}"))
            .pool();
        for _ in 0..32 {
            let mut blocked = false;
            for table in &tables {
                if sqlx::query(&format!("DELETE FROM `{table}`"))
                    .execute(pool)
                    .await
                    .is_err()
                {
                    blocked = true;
                }
            }
            if !blocked {
                return;
            }
        }
        panic!("引导测试库清空未在 32 轮内收敛（存在环状外键？）");
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

    async fn dispatch(app: &BuiltApp, action: &str, body: Value) -> Result<ApiResponse, BaseError> {
        let context = app.context(Request::new(body)).with_request_meta(
            RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], PEER_PORT))),
        );
        app.dispatch_context(
            action_handle(app, action)
                .map_err(|error| BaseError::ConfigError(error.to_string()))?,
            context,
        )
        .await
    }

    /// 经真实注册路径创建一个账号：申请邮箱验证码 → 提交注册。
    pub async fn register_with_code(
        app: &BuiltApp,
        username: &str,
        email: &str,
    ) -> anyhow::Result<()> {
        let request =
            dispatch(app, "request_registration_email", json!({ "email": email })).await?;
        ensure!(request.code == 0, "验证码申请必须成功: {}", request.message);
        let code = take_registration_code(email)?;
        let registered = dispatch(
            app,
            "register",
            json!({
                "username": username,
                "password": PASSWORD,
                "email": email,
                "email_code": code,
            }),
        )
        .await?;
        ensure!(registered.code == 0, "注册必须成功: {}", registered.message);
        Ok(())
    }

    /// 哨兵表的行数：唯一约束下最多为 1。
    pub async fn count_system_owner_rows(app: &BuiltApp) -> u64 {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_owner")
            .fetch_one(pool_of(app))
            .await
            .context("统计引导哨兵行数失败")
            .unwrap_or_else(|error| panic!("{error}"));
        u64::try_from(count).unwrap_or_else(|error| panic!("哨兵行数为负: {error}"))
    }

    /// 内置全权组的启用成员用户名，按 `user_id` 升序。
    pub async fn system_admin_usernames(app: &BuiltApp) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT u.username FROM user_group ug \
             JOIN permission_group g ON g.id = ug.group_id \
             JOIN users u ON u.id = ug.user_id \
             WHERE g.group_key = ? AND u.status = 'active' \
             ORDER BY ug.user_id",
        )
        .bind(SYSTEM_ADMIN_GROUP_KEY)
        .fetch_all(pool_of(app))
        .await
        .context("读取内置全权组成员失败")
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn pool_of(app: &BuiltApp) -> &sqlx::MySqlPool {
        app.tools()
            .mysql()
            .unwrap_or_else(|error| panic!("引导测试应用必须配置 MySQL: {error}"))
            .pool()
    }
}

/// claimer 的幂等契约：哨兵已存在时第二次 claim 必须降级为 `AlreadyClaimed`
/// 而不是报错——这是 Task 9 注册降级语义的前提，因此先单独钉住。
///
/// `OwnerClaimOutcome` 是 crate 私有类型，外部集成测试无法命名它，故这里用
/// 数据库事实表达同一契约：首次注册恰好产生一行哨兵且该账号进入内置全权组，
/// 第二次注册既不新增哨兵行、也不改变全权组成员。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要真实 MySQL/Redis"]
async fn a_second_claim_returns_already_claimed_instead_of_failing() {
    let app = harness::build_test_app().await;
    harness::reset_business_tables(&app).await;

    harness::register_with_code(&app, "first", "first@example.com")
        .await
        .unwrap_or_else(|error| panic!("首次注册应成功: {error}"));
    assert_eq!(
        harness::count_system_owner_rows(&app).await,
        1,
        "首次注册必须夺到哨兵"
    );
    assert_eq!(
        harness::system_admin_usernames(&app).await,
        vec!["first".to_string()],
        "首个注册账号必须被引导进内置全权组"
    );

    harness::register_with_code(&app, "second", "second@example.com")
        .await
        .unwrap_or_else(|error| panic!("第二次注册不得报错: {error}"));
    assert_eq!(
        harness::count_system_owner_rows(&app).await,
        1,
        "哨兵行不得增加"
    );
    assert_eq!(
        harness::system_admin_usernames(&app).await,
        vec!["first".to_string()],
        "第二个账号不得成为系统管理员"
    );
}
