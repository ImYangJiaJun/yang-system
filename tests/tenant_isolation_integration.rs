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

const PASSWORD: &str = "correct-horse-battery-staple";

fn integration_step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "independent-tenant-step-up-secret-32-bytes",
            "tenant-integration-step-up",
            "tenant-integration-sensitive-actions",
        )
        .unwrap_or_else(|error| panic!("集成测试 Step-up manager 应有效: {error}")),
    )
}

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
    let first = dispatch_once(app, module, action, body.clone(), headers).await;
    let challenge = match first {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        result => return result,
    };
    let authorization = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| *value)
        .ok_or_else(|| BaseError::ConfigError("Step-up 目标请求缺少 authorization".to_string()))?;
    let token = authorization.strip_prefix("Bearer ").ok_or_else(|| {
        BaseError::ConfigError("Step-up 目标请求 authorization 不是 Bearer".to_string())
    })?;
    let claims = app.tools().token()?.verify_token(token)?;
    let username = claims
        .custom
        .get("username")
        .and_then(Value::as_str)
        .ok_or_else(|| BaseError::ConfigError("Step-up Access Token 缺少 username".to_string()))?;
    let completed = dispatch_once(
        app,
        "account.user",
        "step_up_complete",
        json!({
            "challenge": challenge,
            "credentials": { "username": username, "password": PASSWORD }
        }),
        &[("authorization", authorization)],
    )
    .await?;
    if completed.code != 0 {
        return Err(BaseError::ConfigError(format!(
            "租户隔离测试 Step-up 完成失败: code={}",
            completed.code
        )));
    }
    let proof = completed
        .data
        .as_ref()
        .and_then(|data| data.get("proof"))
        .and_then(Value::as_str)
        .ok_or_else(|| BaseError::ConfigError("Step-up 完成响应缺少 proof".to_string()))?
        .to_owned();
    let mut retry_headers = headers.to_vec();
    retry_headers.push(("x-step-up-proof", proof.as_str()));
    dispatch_once(app, module, action, body, &retry_headers).await
}

async fn dispatch_once(
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
        .context("浏览器会话响应缺少 yang_refresh Cookie")
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
    let cache_namespace = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let authorization_cache = AuthorizationVersionCache::new(
        redis.clone(),
        format!("tenant-integration-{cache_namespace}"),
    )?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis)
            .with_registration_email(format!("tenant-email-{cache_namespace}"))
            .extension(authorization_cache)
            .extension(integration_step_up_manager())
            .token(TokenManager::new_symmetric(
                "tenant-isolation-integration-secret",
                Algorithm::HS256,
                "yang-system-tenant-integration".to_string(),
                "yang-system-tenant-api".to_string(),
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
    let definitions = definitions.iter().collect::<Vec<_>>();
    initializer.sync_table_definitions(&definitions).await?;
    Ok(Harness {
        application,
        tools,
        pool,
    })
}

async fn register_user(app: &BuiltApp, username: &str) -> anyhow::Result<i64> {
    let email = format!("{username}@example.test");
    data(
        dispatch(
            app,
            "account.user",
            "request_registration_email",
            json!({ "email": email }),
            &[],
        )
        .await?,
    )?;
    let email_code = take_registration_code(&email)?;
    let registered = data(
        dispatch(
            app,
            "account.user",
            "register",
            json!({
                "username": username,
                "password": PASSWORD,
                "email": email,
                "email_code": email_code,
            }),
            &[],
        )
        .await?,
    )?;
    registered["id"].as_i64().context("注册响应缺少用户 id")
}

async fn login(app: &BuiltApp, username: &str) -> anyhow::Result<LoginSession> {
    let response = dispatch(
        app,
        "account.user",
        "login",
        json!({ "username": username, "password": PASSWORD }),
        &[],
    )
    .await?;
    let refresh_token = refresh_cookie(&response)?;
    let response = data(response)?;
    Ok(LoginSession {
        access_token: response["access_token"]
            .as_str()
            .context("登录响应缺少 access_token")?
            .to_string(),
        refresh_token,
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
    let refresh_cookie = format!("yang_refresh={}", login.refresh_token);
    let refreshed = data(
        dispatch(
            app,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", refresh_cookie.as_str())],
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

async fn database_authz_version(pool: &sqlx::MySqlPool, user_id: i64) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
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
    // tenant-evidence: crud-tenant-injection
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
    // tenant-evidence: crud-own-scope
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
    // tenant-evidence: crud-list-scope
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
    // tenant-evidence: crud-object-id-hidden
    ensure!(
        matches!(cross_get, Err(BaseError::RecordNotFound(_))),
        "A 租户猜测 B 对象 id 必须表现为不存在: {cross_get:?}"
    );
    // tenant-evidence: crud-cross-mutation-zero
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
    // tenant-evidence: crud-explicit-tenant-rejected
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
    // tenant-evidence: crud-tenant-move-rejected
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
    // tenant-evidence: crud-context-switch-rejected
    ensure!(
        matches!(cross_context, Err(BaseError::PermissionDenied(_))),
        "A 身份不得直接选择 B 租户上下文: {cross_context:?}"
    );

    let tenant_b_row: (String, String, bool) =
        sqlx::query_as("SELECT name, status, admin FROM org_user WHERE id = ?")
            .bind(tenant_b_membership)
            .fetch_one(&harness.pool)
            .await?;
    // tenant-evidence: crud-cross-effects-zero
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

async fn run_bypass_matrix(harness: &Harness) -> anyhow::Result<()> {
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let tenant_a = create_tenant_actor(
        &harness.application.runtime,
        &format!("bypass_a_admin_{suffix}"),
        "Bypass Tenant A",
        &format!("BA{suffix}"),
    )
    .await?;
    let tenant_b = create_tenant_actor(
        &harness.application.runtime,
        &format!("bypass_b_admin_{suffix}"),
        "Bypass Tenant B",
        &format!("BB{suffix}"),
    )
    .await?;
    let tenant_a_id = tenant_a.tenant_id.to_string();
    let tenant_a_headers = [
        ("authorization", tenant_a.authorization.as_str()),
        ("x-tenant-id", tenant_a_id.as_str()),
    ];

    let tenant_a_discovery = data(
        dispatch(
            &harness.application.runtime,
            "org.tenant",
            "list",
            json!({ "page": 1, "limit": 100 }),
            &[("authorization", tenant_a.authorization.as_str())],
        )
        .await?,
    )?;
    let tenant_a_discovery_items = tenant_a_discovery["items"]
        .as_array()
        .context("A 租户发现响应缺少 items")?;
    // tenant-evidence: join-user-scope
    ensure!(
        tenant_a_discovery_items
            .iter()
            .any(|item| item["id"] == tenant_a.tenant_id),
        "租户发现 join 必须返回当前用户自己的租户"
    );
    ensure!(
        !tenant_a_discovery_items
            .iter()
            .any(|item| item["id"] == tenant_b.tenant_id),
        "租户发现 join 不得泄露其他用户的租户"
    );
    let tenant_b_discovery = data(
        dispatch(
            &harness.application.runtime,
            "org.tenant",
            "list",
            json!({ "page": 1, "limit": 100 }),
            &[("authorization", tenant_b.authorization.as_str())],
        )
        .await?,
    )?;
    let tenant_b_discovery_items = tenant_b_discovery["items"]
        .as_array()
        .context("B 租户发现响应缺少 items")?;
    ensure!(
        tenant_b_discovery_items
            .iter()
            .any(|item| item["id"] == tenant_b.tenant_id)
            && !tenant_b_discovery_items
                .iter()
                .any(|item| item["id"] == tenant_a.tenant_id),
        "租户发现 join 必须双向隔离"
    );

    let relation_options = data(
        dispatch(
            &harness.application.runtime,
            "org.org",
            "select",
            json!({
                "search": "no-page-match",
                "selected": [tenant_a.tenant_id, tenant_b.tenant_id],
                "filter": {},
                "page": 1,
                "limit": 20
            }),
            &tenant_a_headers,
        )
        .await?,
    )?;
    let relation_items = relation_options["items"]
        .as_array()
        .context("企业关系选择响应缺少 items")?;
    // tenant-evidence: relation-selected-scope
    ensure!(
        relation_items
            .iter()
            .any(|item| item["value"] == tenant_a.tenant_id),
        "selected 批量关系加载必须保留当前租户选项"
    );
    ensure!(
        !relation_items
            .iter()
            .any(|item| item["value"] == tenant_b.tenant_id),
        "selected IN 批量关系加载不得带回其他租户"
    );

    let batch_user_a = register_user(
        &harness.application.runtime,
        &format!("batch_member_a_{suffix}"),
    )
    .await?;
    let batch_user_b = register_user(
        &harness.application.runtime,
        &format!("batch_member_b_{suffix}"),
    )
    .await?;
    let batch_add = dispatch(
        &harness.application.runtime,
        "org.user",
        "add",
        json!([
            {
                "user_user": batch_user_a,
                "name": "Batch A",
                "admin": false,
                "status": "active"
            },
            {
                "user_user": batch_user_b,
                "name": "Batch B",
                "admin": false,
                "status": "active"
            }
        ]),
        &tenant_a_headers,
    )
    .await;
    ensure!(batch_add.is_err(), "成员新增契约不得接受数组形式的批量写入");
    let batch_inserted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM org_user WHERE user_user IN (?, ?)")
            .bind(batch_user_a)
            .bind(batch_user_b)
            .fetch_one(&harness.pool)
            .await?;
    // tenant-evidence: batch-add-rejected
    ensure!(batch_inserted == 0, "被拒绝的批量新增不得产生部分写入");

    let tenant_a_membership = creator_membership_id(&harness.pool, &tenant_a).await?;
    let tenant_b_membership = creator_membership_id(&harness.pool, &tenant_b).await?;
    let membership_before: Vec<(i64, String, String, bool)> = sqlx::query_as(
        "SELECT id, name, status, admin FROM org_user WHERE id IN (?, ?) ORDER BY id",
    )
    .bind(tenant_a_membership)
    .bind(tenant_b_membership)
    .fetch_all(&harness.pool)
    .await?;
    let batch_put = dispatch(
        &harness.application.runtime,
        "org.user",
        "put",
        json!({
            "id": [tenant_a_membership, tenant_b_membership],
            "data": { "name": "Batch Cross Tenant Write" }
        }),
        &tenant_a_headers,
    )
    .await;
    ensure!(batch_put.is_err(), "成员更新契约不得接受数组形式的批量主键");
    let batch_delete = dispatch(
        &harness.application.runtime,
        "org.user",
        "del",
        json!({ "id": [tenant_a_membership, tenant_b_membership] }),
        &tenant_a_headers,
    )
    .await;
    ensure!(
        batch_delete.is_err(),
        "成员删除契约不得接受数组形式的批量主键"
    );
    let membership_after: Vec<(i64, String, String, bool)> = sqlx::query_as(
        "SELECT id, name, status, admin FROM org_user WHERE id IN (?, ?) ORDER BY id",
    )
    .bind(tenant_a_membership)
    .bind(tenant_b_membership)
    .fetch_all(&harness.pool)
    .await?;
    // tenant-evidence: batch-mutation-rejected
    ensure!(
        membership_after == membership_before,
        "被拒绝的批量更新/删除不得改变任一租户记录"
    );

    let trigger_name = format!("force_org_user_failure_{suffix}");
    let create_trigger = format!(
        "CREATE TRIGGER `{trigger_name}` BEFORE INSERT ON `org_user` \
         FOR EACH ROW SIGNAL SQLSTATE '45000' \
         SET MESSAGE_TEXT = 'forced tenant onboarding membership failure'"
    );
    sqlx::raw_sql(&create_trigger)
        .execute(&harness.pool)
        .await?;
    let rollback_code = format!("ROLLBACK{suffix}");
    let creator_version_before_failed_onboarding =
        database_authz_version(&harness.pool, tenant_a.user_id).await?;
    let outbox_before_failed_onboarding: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ?")
            .bind(tenant_a.user_id)
            .fetch_one(&harness.pool)
            .await?;
    let failed_onboarding = dispatch(
        &harness.application.runtime,
        "org.tenant",
        "create",
        json!({ "name": "Must Roll Back", "code": rollback_code }),
        &[("authorization", tenant_a.authorization.as_str())],
    )
    .await;
    let drop_trigger = format!("DROP TRIGGER IF EXISTS `{trigger_name}`");
    sqlx::raw_sql(&drop_trigger).execute(&harness.pool).await?;
    ensure!(
        failed_onboarding.is_err(),
        "测试触发器必须强制租户创建第二步失败"
    );
    let rolled_back_orgs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM org_org WHERE code = ?")
        .bind(&rollback_code)
        .fetch_one(&harness.pool)
        .await?;
    // tenant-evidence: transaction-rollback
    ensure!(
        rolled_back_orgs == 0,
        "首个成员插入失败时，事务必须回滚已插入的企业"
    );
    ensure!(
        database_authz_version(&harness.pool, tenant_a.user_id).await?
            == creator_version_before_failed_onboarding,
        "onboarding 失败事务不得递增创建者授权版本"
    );
    let outbox_after_failed_onboarding: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ?")
            .bind(tenant_a.user_id)
            .fetch_one(&harness.pool)
            .await?;
    ensure!(
        outbox_after_failed_onboarding == outbox_before_failed_onboarding,
        "onboarding 失败事务不得遗留未提交的授权 Outbox 事件"
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn tenant_join_relation_batch_and_transaction_bypasses_are_closed() -> anyhow::Result<()> {
    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "集成测试 Redis URL 必须使用独立 DB 15"
    );

    let harness = build_harness(&mysql_url, &redis_url).await?;
    let outcome = run_bypass_matrix(&harness).await;
    let cleanup = reset_test_database(&harness.pool).await;
    harness.tools.close().await;
    merge_outcome(outcome, cleanup)
}
