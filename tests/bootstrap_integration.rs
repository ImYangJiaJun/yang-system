mod common;

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::database::DatabaseInitializer;
use yang_base::definition::{ActionHandle, ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::{Tools, ToolsBuilder};
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::{build_app, Application};
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::SecuritySettings;

use common::{take_registration_code, RegistrationEmailToolsExt};

fn integration_step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "independent-owner-step-up-secret-32-bytes",
            "owner-integration-step-up",
            "owner-integration-sensitive-actions",
        )
        .unwrap_or_else(|error| panic!("集成测试 Step-up manager 应有效: {error}")),
    )
}

fn action_handle(app: &BuiltApp, module: &str, action: &str) -> Result<ActionHandle, BaseError> {
    let module = ModuleName::new(module)
        .map_err(|error| BaseError::ConfigError(format!("ModuleName 无效: {error}")))?;
    let action = ActionName::new(action)
        .map_err(|error| BaseError::ConfigError(format!("ActionName 无效: {error}")))?;
    let reference = ActionRef::new(module, action);
    app.registry()
        .resolve(&reference)
        .ok_or_else(|| BaseError::ConfigError(format!("测试 Action 未注册: {reference}")))
}

async fn dispatch(
    app: &BuiltApp,
    module: &str,
    action: &str,
    body: Value,
) -> Result<ApiResponse, BaseError> {
    let peer: SocketAddr = "127.0.0.1:42000"
        .parse()
        .map_err(|error| BaseError::ConfigError(format!("测试 peer 地址无效: {error}")))?;
    let context = app
        .context(Request::new(body))
        .with_request_meta(RequestMeta::new().with_peer_addr(peer));
    app.dispatch_context(action_handle(app, module, action)?, context)
        .await
}

fn data(response: ApiResponse) -> anyhow::Result<Value> {
    ensure!(
        response.code == 0,
        "Action 返回业务错误 {}: {}",
        response.code,
        response.message
    );
    response.data.context("Action 成功响应缺少 data")
}

async fn reset_test_database(pool: &sqlx::MySqlPool) -> anyhow::Result<()> {
    let database: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(pool)
        .await?;
    let database = database.context("测试连接没有选择数据库")?;
    ensure!(
        database.ends_with("_test"),
        "拒绝清理非测试数据库 {database:?}；数据库名必须以 _test 结尾"
    );
    for table in [
        "password_reset_token",
        "audit_event",
        "authorization_outbox",
        "work_task",
        "work_project",
        "org_user",
        "org_org",
        "admin_user",
        "users",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn build_harness(
    mysql_url: &str,
    redis_url: &str,
) -> anyhow::Result<(Application, Arc<Tools>, sqlx::MySqlPool)> {
    let database_config = DatabaseConfig::default()
        .with_max_connections(8)
        .with_min_connections(0)
        .with_connect_timeout(10);
    let mysql = Database::connect_with_config(mysql_url, database_config.clone())
        .await
        .context("连接最终管理员测试 MySQL 失败")?;
    let pool = mysql.pool().clone();
    reset_test_database(&pool).await?;
    let initializer_database = Database::from_pool(pool.clone(), database_config)?;
    let redis = RedisClient::connect_with_config(
        redis_url,
        RedisConfig::default()
            .with_max_connections(4)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await
    .context("连接最终管理员测试 Redis 失败")?;
    let cache_namespace = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let authorization_cache = AuthorizationVersionCache::new(
        redis.clone(),
        format!("owner-integration-{cache_namespace}"),
    )?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis)
            .with_registration_email(format!("owner-email-{cache_namespace}"))
            .extension(authorization_cache)
            .extension(integration_step_up_manager())
            .token(TokenManager::new_symmetric(
                "owner-integration-token-secret",
                Algorithm::HS256,
                "yang-system-owner-integration".to_string(),
                "yang-system-owner-api".to_string(),
                300,
                3600,
            ))
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 1_000,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: false,
        trusted_proxy_cidrs: Vec::new(),
    });
    let application = build_app(Arc::clone(&tools), security)?;
    let initializer = DatabaseInitializer::new(initializer_database);
    let definitions = yang_system::schema::definitions(&application.runtime)?;
    initializer
        .sync_table_definitions(&definitions.iter().collect::<Vec<_>>())
        .await?;
    Ok((application, tools, pool))
}

async fn registration_body(
    app: &BuiltApp,
    username: &str,
    password: &str,
) -> anyhow::Result<Value> {
    let email = format!("{username}@example.test");
    data(
        dispatch(
            app,
            "account.user",
            "request_registration_email",
            json!({ "email": email }),
        )
        .await?,
    )?;
    let email_code = take_registration_code(&email)?;
    Ok(json!({
        "username": username,
        "password": password,
        "email": email,
        "email_code": email_code,
    }))
}

async fn login(app: &BuiltApp, username: &str, password: &str) -> anyhow::Result<String> {
    let response = data(
        dispatch(
            app,
            "account.user",
            "login",
            json!({ "username": username, "password": password }),
        )
        .await?,
    )?;
    response["access_token"]
        .as_str()
        .map(str::to_string)
        .context("登录响应缺少 access_token")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn first_concurrent_registration_claims_exactly_one_permanent_owner() -> anyhow::Result<()> {
    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "集成测试 Redis URL 必须使用独立 DB 15"
    );

    let (application, tools, pool) = build_harness(&mysql_url, &redis_url).await?;
    let outcome = async {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let first_username = format!("owner_a_{suffix}");
        let second_username = format!("owner_b_{suffix}");
        let password = "correct-horse-battery-staple";
        let first_body = registration_body(&application.runtime, &first_username, password).await?;
        let second_body =
            registration_body(&application.runtime, &second_username, password).await?;

        let first = dispatch(&application.runtime, "account.user", "register", first_body);
        let second = dispatch(
            &application.runtime,
            "account.user",
            "register",
            second_body,
        );
        let (first, second) = tokio::join!(first, second);
        let first = data(first?)?;
        let second = data(second?)?;
        let user_ids = [
            first["id"].as_i64().context("首个响应缺少 id")?,
            second["id"].as_i64().context("第二个响应缺少 id")?,
        ];

        let owners: Vec<(i64, String, bool, String)> = sqlx::query_as(
            "SELECT user_user, status, admin, owner_key FROM admin_user \
             WHERE owner_key IS NOT NULL",
        )
        .fetch_all(&pool)
        .await?;
        ensure!(
            owners.len() == 1,
            "并发注册后必须恰好一个 owner: {owners:?}"
        );
        ensure!(
            user_ids.contains(&owners[0].0)
                && owners[0].1 == "active"
                && owners[0].2
                && owners[0].3 == "system-owner",
            "owner 必须绑定成功注册用户并保持永久管理员不变量: {owners:?}"
        );
        let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await?;
        ensure!(user_count == 2, "owner 竞争失败不得导致普通注册回滚");

        let duplicate = sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, status, admin, owner_key, created_at, updated_at) \
             VALUES (?, 'duplicate-owner', 'active', TRUE, 'system-owner', 1, 1)",
        )
        .bind(
            user_ids
                .into_iter()
                .find(|id| *id != owners[0].0)
                .context("缺少普通用户")?,
        )
        .execute(&pool)
        .await;
        ensure!(duplicate.is_err(), "数据库唯一约束必须拒绝第二个 owner");

        let first_token = login(&application.runtime, &first_username, password).await?;
        let second_token = login(&application.runtime, &second_username, password).await?;
        let first_roles = tools.token()?.verify_token(&first_token)?.custom["roles"].clone();
        let second_roles = tools.token()?.verify_token(&second_token)?.custom["roles"].clone();
        let owner_role_count = [first_roles, second_roles]
            .iter()
            .filter(|roles| {
                roles
                    .as_array()
                    .is_some_and(|roles| roles.iter().any(|role| role == "system_owner"))
            })
            .count();
        ensure!(
            owner_role_count == 1,
            "只有 owner Token 可以携带 system_owner 角色"
        );

        let bootstrap_ref = ActionRef::new(
            ModuleName::new("admin.user")
                .map_err(|error| anyhow::anyhow!("ModuleName 无效: {error}"))?,
            ActionName::new("bootstrap")
                .map_err(|error| anyhow::anyhow!("ActionName 无效: {error}"))?,
        );
        ensure!(
            application
                .runtime
                .registry()
                .resolve(&bootstrap_ref)
                .is_none(),
            "系统不得保留独立管理员初始化 Action"
        );
        let owner_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_event \
             WHERE action = 'account.user.register' \
               AND JSON_EXTRACT(after_summary, '$.system_owner') = TRUE",
        )
        .fetch_one(&pool)
        .await?;
        ensure!(owner_audits == 1, "owner 声明必须且只能产生一条成功审计");
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&pool).await;
    tools.close().await;
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("测试失败后清理也失败: {cleanup_error:#}")))
        }
    }
}
