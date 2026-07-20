use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta};
use yang_base::database::DatabaseInitializer;
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::config::SecuritySettings;

fn action_handle(
    app: &BuiltApp,
    module: &str,
    action: &str,
) -> anyhow::Result<yang_base::definition::ActionHandle> {
    let module =
        ModuleName::new(module).map_err(|error| anyhow::anyhow!("ModuleName 无效: {error}"))?;
    let action =
        ActionName::new(action).map_err(|error| anyhow::anyhow!("ActionName 无效: {error}"))?;
    let reference = ActionRef::new(module, action);
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
    query: &[(&str, &str)],
) -> anyhow::Result<ApiResponse> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    for (name, value) in query {
        request = request.query(*name, *value);
    }
    let peer: SocketAddr = "127.0.0.1:41000".parse()?;
    let context = app
        .context(request)
        .with_request_meta(RequestMeta::new().with_peer_addr(peer));
    app.dispatch_context(action_handle(app, module, action)?, context)
        .await
        .map_err(|error| anyhow::anyhow!("{module}.{action} 调用失败: {error}"))
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
    for table in ["org_user", "org_org", "admin_user", "users"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn real_mysql_redis_support_account_and_tenant_lifecycle() -> anyhow::Result<()> {
    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "集成测试 Redis URL 必须使用独立 DB 15"
    );

    let database_config = DatabaseConfig::default()
        .with_max_connections(4)
        .with_min_connections(0)
        .with_connect_timeout(10);
    let mysql = Database::connect_with_config(&mysql_url, database_config.clone())
        .await
        .context("连接测试 MySQL 失败")?;
    reset_test_database(mysql.pool()).await?;
    let initializer_database = Database::from_pool(mysql.pool().clone(), database_config)?;
    let redis = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(4)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await
    .context("连接测试 Redis 失败")?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis)
            .token(TokenManager::new_symmetric(
                "integration-test-secret-32-bytes-minimum",
                Algorithm::HS256,
                "yang-system-integration".to_string(),
                "yang-system-integration-api".to_string(),
                300,
                3600,
            ))
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 100,
    });
    let application = build_app(Arc::clone(&tools), security)?;
    let initializer = DatabaseInitializer::new(initializer_database, false);
    let definitions = application
        .runtime
        .table_definitions()
        .iter()
        .collect::<Vec<_>>();

    let pending = initializer.plan_table_definitions(&definitions).await?;
    ensure!(!pending.is_noop(), "空测试数据库应产生 schema 变更计划");
    initializer.sync_table_definitions(&definitions).await?;
    ensure!(
        initializer
            .plan_table_definitions(&definitions)
            .await?
            .is_noop(),
        "同步后 schema 规划必须为空"
    );

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let username = format!("integration_{suffix}");
    let password = "correct-horse-battery-staple";
    data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({ "username": username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;

    let login = data(
        dispatch(
            &application.runtime,
            "account.user",
            "login",
            json!({ "username": username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let access_token = login["access_token"]
        .as_str()
        .context("登录响应缺少 access_token")?;
    let refresh_token = login["refresh_token"]
        .as_str()
        .context("登录响应缺少 refresh_token")?;
    data(
        dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({ "name": "Integration Administrator", "position": "Owner" }),
            &[("authorization", &format!("Bearer {access_token}"))],
            &[],
        )
        .await?,
    )?;
    ensure!(
        dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({ "name": "Second Administrator" }),
            &[("authorization", &format!("Bearer {access_token}"))],
            &[],
        )
        .await
        .is_err(),
        "平台管理员初始化必须只能成功一次"
    );

    let refreshed_admin = data(
        dispatch(
            &application.runtime,
            "account.user",
            "refresh",
            json!({ "refresh_token": refresh_token }),
            &[],
            &[],
        )
        .await?,
    )?;
    let admin_access_token = refreshed_admin["access_token"]
        .as_str()
        .context("平台管理员刷新响应缺少 access_token")?;
    let admin_refresh_token = refreshed_admin["refresh_token"]
        .as_str()
        .context("平台管理员刷新响应缺少 refresh_token")?;
    let admin_authorization = format!("Bearer {admin_access_token}");
    let admin_accounts = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "list",
            json!({}),
            &[("authorization", &admin_authorization)],
            &[("page", "1"), ("limit", "20")],
        )
        .await?,
    )?;
    ensure!(
        admin_accounts["items"]
            .as_array()
            .is_some_and(|items| items.len() == 1),
        "刷新 Token 后应获得平台账号读取权限"
    );

    let organization = data(
        dispatch(
            &application.runtime,
            "org.tenant",
            "create",
            json!({ "name": "Integration Corp", "code": format!("IT{suffix}") }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let organization_id = organization["id"].as_i64().context("创建企业响应缺少 id")?;

    let member_username = format!("member_{suffix}");
    let member = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({ "username": member_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let member_id = member["id"].as_i64().context("成员注册响应缺少 id")?;
    let tenant_id = organization_id.to_string();
    let member_body = json!({
        "org_org": organization_id,
        "user_user": member_id,
        "name": "Integration Member",
        "admin": false,
        "status": "active"
    });
    ensure!(
        dispatch(
            &application.runtime,
            "org.user",
            "add",
            member_body.clone(),
            &[
                ("authorization", &admin_authorization),
                ("x-tenant-id", &tenant_id),
            ],
            &[],
        )
        .await
        .is_err(),
        "创建企业前签发的 Token 不应隐式获得组织写权限"
    );
    let refreshed_org_admin = data(
        dispatch(
            &application.runtime,
            "account.user",
            "refresh",
            json!({ "refresh_token": admin_refresh_token }),
            &[],
            &[],
        )
        .await?,
    )?;
    let org_access_token = refreshed_org_admin["access_token"]
        .as_str()
        .context("组织管理员刷新响应缺少 access_token")?;
    let authorization = format!("Bearer {org_access_token}");
    data(
        dispatch(
            &application.runtime,
            "org.user",
            "add",
            member_body,
            &[
                ("authorization", &authorization),
                ("x-tenant-id", &tenant_id),
            ],
            &[],
        )
        .await?,
    )?;

    let tenants = data(
        dispatch(
            &application.runtime,
            "org.tenant",
            "list",
            json!({}),
            &[("authorization", &authorization)],
            &[("page", "1"), ("limit", "20")],
        )
        .await?,
    )?;
    ensure!(
        tenants["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == organization_id)),
        "租户发现未返回新创建企业"
    );

    let organizations = data(
        dispatch(
            &application.runtime,
            "org.org",
            "list",
            json!({}),
            &[
                ("authorization", &authorization),
                ("x-tenant-id", &tenant_id),
            ],
            &[("page", "1"), ("limit", "20")],
        )
        .await?,
    )?;
    ensure!(
        organizations["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == organization_id)),
        "租户作用域企业列表未返回当前企业"
    );

    tools.close().await;
    Ok(())
}
