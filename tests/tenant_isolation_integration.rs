use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta};
use yang_base::database::DatabaseInitializer;
use yang_base::definition::{ActionHandle, ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::{Tools, ToolsBuilder};
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::{build_app, Application};
use yang_system::bootstrap_secret::{generate_bootstrap_secret, BootstrapSecretVerifier};
use yang_system::config::SecuritySettings;

const PASSWORD: &str = "correct-horse-battery-staple";

struct Harness {
    application: Application,
    tools: Arc<Tools>,
    pool: sqlx::MySqlPool,
}

struct LoginSession {
    access_token: String,
    refresh_token: String,
}

struct TenantActor {
    user_id: i64,
    tenant_id: i64,
    authorization: String,
}

fn action_handle(app: &BuiltApp, module: &str, action: &str) -> Result<ActionHandle, BaseError> {
    let module = ModuleName::new(module)
        .map_err(|error| BaseError::ConfigError(format!("ModuleName 无效: {error}")))?;
    let action = ActionName::new(action)
        .map_err(|error| BaseError::ConfigError(format!("ActionName 无效: {error}")))?;
    let reference = ActionRef::new(module, action);
    app.registry().resolve(&reference).ok_or_else(|| {
        BaseError::ConfigError(format!("租户隔离集成测试 Action 未注册: {reference}"))
    })
}

async fn dispatch(
    app: &BuiltApp,
    module: &str,
    action: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let peer: SocketAddr = "127.0.0.1:43000"
        .parse()
        .map_err(|error| BaseError::ConfigError(format!("测试 peer 地址无效: {error}")))?;
    let context = app
        .context(request)
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
    for table in ["org_user", "org_org", "admin_user", "users"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn build_harness(mysql_url: &str, redis_url: &str) -> anyhow::Result<Harness> {
    let database_config = DatabaseConfig::default()
        .with_max_connections(8)
        .with_min_connections(0)
        .with_connect_timeout(10);
    let mysql = Database::connect_with_config(mysql_url, database_config.clone())
        .await
        .context("连接租户隔离测试 MySQL 失败")?;
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
    .context("连接租户隔离测试 Redis 失败")?;
    let generated_bootstrap = generate_bootstrap_secret()?;
    let verifier = BootstrapSecretVerifier::new(generated_bootstrap.digest().clone(), 2)?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis)
            .token(TokenManager::new_symmetric(
                "tenant-isolation-integration-secret",
                Algorithm::HS256,
                "yang-system-tenant-integration".to_string(),
                "yang-system-tenant-api".to_string(),
                300,
                3600,
            ))
            .config(verifier)
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 1_000,
    });
    let application = build_app(Arc::clone(&tools), security)?;
    let initializer = DatabaseInitializer::new(initializer_database, false);
    let definitions = application
        .runtime
        .table_definitions()
        .iter()
        .collect::<Vec<_>>();
    initializer.sync_table_definitions(&definitions).await?;
    Ok(Harness {
        application,
        tools,
        pool,
    })
}

async fn register_user(app: &BuiltApp, username: &str) -> anyhow::Result<i64> {
    let registered = data(
        dispatch(
            app,
            "account.user",
            "register",
            json!({ "username": username, "password": PASSWORD }),
            &[],
        )
        .await?,
    )?;
    registered["id"].as_i64().context("注册响应缺少用户 id")
}

async fn login(app: &BuiltApp, username: &str) -> anyhow::Result<LoginSession> {
    let response = data(
        dispatch(
            app,
            "account.user",
            "login",
            json!({ "username": username, "password": PASSWORD }),
            &[],
        )
        .await?,
    )?;
    Ok(LoginSession {
        access_token: response["access_token"]
            .as_str()
            .context("登录响应缺少 access_token")?
            .to_string(),
        refresh_token: response["refresh_token"]
            .as_str()
            .context("登录响应缺少 refresh_token")?
            .to_string(),
    })
}

async fn create_tenant_actor(
    app: &BuiltApp,
    username: &str,
    tenant_name: &str,
    tenant_code: &str,
) -> anyhow::Result<TenantActor> {
    let user_id = register_user(app, username).await?;
    let login = login(app, username).await?;
    let initial_authorization = format!("Bearer {}", login.access_token);
    let tenant = data(
        dispatch(
            app,
            "org.tenant",
            "create",
            json!({ "name": tenant_name, "code": tenant_code }),
            &[("authorization", initial_authorization.as_str())],
        )
        .await?,
    )?;
    let tenant_id = tenant["id"].as_i64().context("创建企业响应缺少 id")?;
    let refreshed = data(
        dispatch(
            app,
            "account.user",
            "refresh",
            json!({ "refresh_token": login.refresh_token }),
            &[],
        )
        .await?,
    )?;
    let access_token = refreshed["access_token"]
        .as_str()
        .context("刷新响应缺少 access_token")?;
    Ok(TenantActor {
        user_id,
        tenant_id,
        authorization: format!("Bearer {access_token}"),
    })
}

async fn creator_membership_id(pool: &sqlx::MySqlPool, actor: &TenantActor) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT id FROM org_user WHERE org_org = ? AND user_user = ?")
        .bind(actor.tenant_id)
        .bind(actor.user_id)
        .fetch_one(pool)
        .await
        .context("缺少租户创建者成员记录")
}

fn affected(response: ApiResponse) -> anyhow::Result<u64> {
    data(response)?["affected"]
        .as_u64()
        .context("CRUD 响应缺少 affected")
}

fn merge_outcome(outcome: anyhow::Result<()>, cleanup: anyhow::Result<()>) -> anyhow::Result<()> {
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("租户隔离测试失败后清理也失败: {cleanup_error:#}")))
        }
    }
}

async fn run_isolation_matrix(harness: &Harness) -> anyhow::Result<()> {
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let tenant_a = create_tenant_actor(
        &harness.application.runtime,
        &format!("tenant_a_admin_{suffix}"),
        "Tenant A",
        &format!("TA{suffix}"),
    )
    .await?;
    let tenant_b = create_tenant_actor(
        &harness.application.runtime,
        &format!("tenant_b_admin_{suffix}"),
        "Tenant B",
        &format!("TB{suffix}"),
    )
    .await?;
    let tenant_a_id = tenant_a.tenant_id.to_string();
    let tenant_b_id = tenant_b.tenant_id.to_string();
    let tenant_a_headers = [
        ("authorization", tenant_a.authorization.as_str()),
        ("x-tenant-id", tenant_a_id.as_str()),
    ];
    let tenant_b_headers = [
        ("authorization", tenant_b.authorization.as_str()),
        ("x-tenant-id", tenant_b_id.as_str()),
    ];
    let tenant_b_membership = creator_membership_id(&harness.pool, &tenant_b).await?;
    let tenant_b_before: (String, String, bool) =
        sqlx::query_as("SELECT name, status, admin FROM org_user WHERE id = ?")
            .bind(tenant_b_membership)
            .fetch_one(&harness.pool)
            .await?;

    let member_user_id = register_user(
        &harness.application.runtime,
        &format!("tenant_a_member_{suffix}"),
    )
    .await?;
    let inserted = data(
        dispatch(
            &harness.application.runtime,
            "org.user",
            "add",
            json!({
                "user_user": member_user_id,
                "name": "Tenant A Member",
                "admin": false,
                "status": "active"
            }),
            &tenant_a_headers,
        )
        .await?,
    )?;
    ensure!(inserted["affected"] == 1, "租户内新增必须影响一行");
    let tenant_a_membership = inserted["id"].as_i64().context("新增成员响应缺少 id")?;
    let stored_tenant: i64 = sqlx::query_scalar("SELECT org_org FROM org_user WHERE id = ?")
        .bind(tenant_a_membership)
        .fetch_one(&harness.pool)
        .await?;
    ensure!(
        stored_tenant == tenant_a.tenant_id,
        "tenant key 必须由可信上下文注入"
    );

    let own = data(
        dispatch(
            &harness.application.runtime,
            "org.user",
            "get",
            json!({ "id": tenant_a_membership }),
            &tenant_a_headers,
        )
        .await?,
    )?;
    ensure!(own["id"] == tenant_a_membership, "租户内主键读取必须成功");
    ensure!(
        affected(
            dispatch(
                &harness.application.runtime,
                "org.user",
                "put",
                json!({ "id": tenant_a_membership, "data": { "name": "Tenant A Updated" } }),
                &tenant_a_headers,
            )
            .await?,
        )? == 1,
        "租户内主键更新必须影响一行"
    );

    let selected = data(
        dispatch(
            &harness.application.runtime,
            "org.user",
            "select",
            json!({ "page": 1, "page_size": 100, "count_total": true }),
            &tenant_a_headers,
        )
        .await?,
    )?;
    let items = selected["items"].as_array().context("成员列表缺少 items")?;
    ensure!(
        items
            .iter()
            .all(|item| item["org_org"] == tenant_a.tenant_id),
        "A 租户列表不得出现 B 租户记录: {items:?}"
    );
    ensure!(
        !items.iter().any(|item| item["id"] == tenant_b_membership),
        "A 租户列表不得泄露 B 租户对象 id"
    );

    let cross_get = dispatch(
        &harness.application.runtime,
        "org.user",
        "get",
        json!({ "id": tenant_b_membership }),
        &tenant_a_headers,
    )
    .await;
    ensure!(
        matches!(cross_get, Err(BaseError::RecordNotFound(_))),
        "A 租户猜测 B 对象 id 必须表现为不存在: {cross_get:?}"
    );
    ensure!(
        affected(
            dispatch(
                &harness.application.runtime,
                "org.user",
                "put",
                json!({ "id": tenant_b_membership, "data": { "name": "Cross Tenant Write" } }),
                &tenant_a_headers,
            )
            .await?,
        )? == 0,
        "A 租户更新 B 对象必须影响零行"
    );
    ensure!(
        affected(
            dispatch(
                &harness.application.runtime,
                "org.user",
                "del",
                json!({ "id": tenant_b_membership }),
                &tenant_a_headers,
            )
            .await?,
        )? == 0,
        "A 租户删除 B 对象必须影响零行"
    );

    let explicit_tenant_user = register_user(
        &harness.application.runtime,
        &format!("explicit_tenant_member_{suffix}"),
    )
    .await?;
    let explicit_insert = dispatch(
        &harness.application.runtime,
        "org.user",
        "add",
        json!({
            "org_org": tenant_b.tenant_id,
            "user_user": explicit_tenant_user,
            "name": "Explicit Tenant",
            "admin": false,
            "status": "active"
        }),
        &tenant_a_headers,
    )
    .await;
    ensure!(
        matches!(explicit_insert, Err(BaseError::PermissionDenied(_))),
        "调用方显式写 tenant key 必须失败: {explicit_insert:?}"
    );
    let explicit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM org_user WHERE user_user = ?")
            .bind(explicit_tenant_user)
            .fetch_one(&harness.pool)
            .await?;
    ensure!(explicit_count == 0, "失败的显式 tenant 写不得落库");

    let move_tenant = dispatch(
        &harness.application.runtime,
        "org.user",
        "put",
        json!({ "id": tenant_a_membership, "data": { "org_org": tenant_b.tenant_id } }),
        &tenant_a_headers,
    )
    .await;
    ensure!(
        move_tenant.is_err(),
        "更新路径不得允许把 A 对象移动到 B 租户"
    );
    let stored_tenant_after_move: i64 =
        sqlx::query_scalar("SELECT org_org FROM org_user WHERE id = ?")
            .bind(tenant_a_membership)
            .fetch_one(&harness.pool)
            .await?;
    ensure!(
        stored_tenant_after_move == tenant_a.tenant_id,
        "失败的租户迁移不得改变数据归属"
    );

    let cross_context_headers = [
        ("authorization", tenant_a.authorization.as_str()),
        ("x-tenant-id", tenant_b_id.as_str()),
    ];
    let cross_context = dispatch(
        &harness.application.runtime,
        "org.user",
        "select",
        json!({ "page": 1, "page_size": 100 }),
        &cross_context_headers,
    )
    .await;
    ensure!(
        matches!(cross_context, Err(BaseError::PermissionDenied(_))),
        "A 身份不得直接选择 B 租户上下文: {cross_context:?}"
    );

    let tenant_b_row: (String, String, bool) =
        sqlx::query_as("SELECT name, status, admin FROM org_user WHERE id = ?")
            .bind(tenant_b_membership)
            .fetch_one(&harness.pool)
            .await?;
    ensure!(
        tenant_b_row == tenant_b_before,
        "跨租户更新/删除不得改变 B 创建者成员记录: before={tenant_b_before:?}, after={tenant_b_row:?}"
    );

    ensure!(
        affected(
            dispatch(
                &harness.application.runtime,
                "org.user",
                "del",
                json!({ "id": tenant_a_membership }),
                &tenant_a_headers,
            )
            .await?,
        )? == 1,
        "租户内删除正向对照必须影响一行"
    );
    let tenant_b_visible = data(
        dispatch(
            &harness.application.runtime,
            "org.user",
            "get",
            json!({ "id": tenant_b_membership }),
            &tenant_b_headers,
        )
        .await?,
    )?;
    ensure!(
        tenant_b_visible["id"] == tenant_b_membership,
        "B 租户仍应读取自己的成员对象"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn tenant_crud_and_object_ids_are_isolated_end_to_end() -> anyhow::Result<()> {
    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "集成测试 Redis URL 必须使用独立 DB 15"
    );

    let harness = build_harness(&mysql_url, &redis_url).await?;
    let outcome = run_isolation_matrix(&harness).await;
    let cleanup = reset_test_database(&harness.pool).await;
    harness.tools.close().await;
    merge_outcome(outcome, cleanup)
}
