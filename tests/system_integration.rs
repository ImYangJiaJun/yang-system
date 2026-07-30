use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta};
use yang_base::database::DatabaseInitializer;
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::{
    AuthorizationOutboxWorker, AuthorizationVersionCache, CachedAuthorizationVersion,
};
use yang_system::bootstrap_secret::{generate_bootstrap_secret, BootstrapSecretVerifier};
use yang_system::config::{AuthorizationSettings, SecuritySettings};

fn integration_token_manager() -> TokenManager {
    TokenManager::new_symmetric_keyring(
        "integration-active".to_string(),
        "integration-test-secret-32-bytes-minimum",
        Vec::new(),
        Algorithm::HS256,
        "yang-system-integration".to_string(),
        "yang-system-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("集成测试 Token keyring 应构建成功: {error}"))
}

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

async fn dispatch_token_action(
    app: &BuiltApp,
    module: &str,
    action: &str,
    token: &str,
) -> Result<ApiResponse, BaseError> {
    dispatch_token_body_action(app, module, action, token, json!({})).await
}

async fn dispatch_token_body_action(
    app: &BuiltApp,
    module: &str,
    action: &str,
    token: &str,
    body: Value,
) -> Result<ApiResponse, BaseError> {
    let request = Request::new(body).header("authorization", format!("Bearer {token}"));
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], 41_001))),
    );
    let handle = action_handle(app, module, action)
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    app.dispatch_context(handle, context).await
}

async fn assert_authorization_error(
    app: &BuiltApp,
    module: &str,
    action: &str,
    token: &str,
    expected_code: i32,
) -> anyhow::Result<()> {
    match dispatch_token_action(app, module, action, token).await {
        Err(error) if error.code() == expected_code => Ok(()),
        Err(error) => anyhow::bail!(
            "授权错误码不符: expected={expected_code}, actual={}, error={error}",
            error.code()
        ),
        Ok(response) => anyhow::bail!(
            "预期授权失败 {expected_code}，实际 Action 成功: code={}, message={}",
            response.code,
            response.message
        ),
    }
}

async fn assert_authorization_error_with_body(
    app: &BuiltApp,
    module: &str,
    action: &str,
    token: &str,
    body: Value,
    expected_code: i32,
) -> anyhow::Result<()> {
    match dispatch_token_body_action(app, module, action, token, body).await {
        Err(error) if error.code() == expected_code => Ok(()),
        Err(error) => anyhow::bail!(
            "授权错误码不符: expected={expected_code}, actual={}, error={error}",
            error.code()
        ),
        Ok(response) => anyhow::bail!(
            "预期授权失败 {expected_code}，实际 Action 成功: code={}, message={}",
            response.code,
            response.message
        ),
    }
}

async fn assert_authorization_success(
    app: &BuiltApp,
    module: &str,
    action: &str,
    token: &str,
) -> anyhow::Result<()> {
    let response = dispatch_token_action(app, module, action, token).await?;
    ensure!(
        response.code == 0,
        "预期授权成功，实际 Action 返回业务错误 {}: {}",
        response.code,
        response.message
    );
    Ok(())
}

async fn wait_for_cached_version(
    cache: &AuthorizationVersionCache,
    user_id: i64,
    expected: i64,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if cache.read(user_id).await? == CachedAuthorizationVersion::Version(expected) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "等待授权缓存超时: user_id={user_id}, expected={expected}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn authorization_cache_key(deployment: &str, user_id: i64) -> String {
    format!("yang-system:{deployment}:authz:version:{user_id}")
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

fn token_authz_version(tools: &yang_base::tools::Tools, token: &str) -> anyhow::Result<i64> {
    tools
        .token()?
        .verify_token(token)?
        .custom
        .get("authz_version")
        .and_then(Value::as_i64)
        .filter(|version| *version >= 1)
        .context("Token 缺少正整数 authz_version")
}

async fn database_authz_version(pool: &sqlx::MySqlPool, user_id: i64) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

async fn wait_for_outbox_idle(pool: &sqlx::MySqlPool) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM authorization_outbox WHERE state <> 'published'",
        )
        .fetch_one(pool)
        .await?;
        if remaining == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "等待授权 Outbox 清空超时: remaining={remaining}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
        "audit_event",
        "authorization_outbox",
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
    let initializer_database = Database::from_pool(mysql.pool().clone(), database_config.clone())?;
    let redis = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(4)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await
    .context("连接测试 Redis 失败")?;
    let cache_namespace = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let deployment = format!("system-integration-{cache_namespace}");
    let authorization_cache = AuthorizationVersionCache::new(redis.clone(), deployment.clone())?;
    let authorization_cache_probe = authorization_cache.clone();
    let generated_bootstrap = generate_bootstrap_secret()?;
    let bootstrap_secret = generated_bootstrap.secret().to_owned();
    let bootstrap_verifier = BootstrapSecretVerifier::new(generated_bootstrap.digest().clone(), 2)?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis.clone())
            .extension(authorization_cache)
            .token(integration_token_manager())
            .config(bootstrap_verifier)
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 100,
        trusted_proxy_cidrs: Vec::new(),
    });
    let application = build_app(Arc::clone(&tools), Arc::clone(&security))?;
    let initializer = DatabaseInitializer::new(initializer_database, false);
    let definitions = application
        .runtime
        .table_definitions()
        .iter()
        .collect::<Vec<_>>();

    let pending = initializer.plan_table_definitions(&definitions).await?;
    ensure!(!pending.is_noop(), "空测试数据库应产生 schema 变更计划");
    initializer.sync_table_definitions(&definitions).await?;
    sqlx::raw_sql(include_str!(
        "../migrations/20260726_0006_create_authorization_outbox.sql"
    ))
    .execute(tools.mysql()?.pool())
    .await?;
    sqlx::raw_sql(include_str!(
        "../migrations/20260726_0007_create_audit_event.sql"
    ))
    .execute(tools.mysql()?.pool())
    .await?;
    ensure!(
        initializer
            .plan_table_definitions(&definitions)
            .await?
            .is_noop(),
        "同步后 schema 规划必须为空"
    );
    let outbox_worker = AuthorizationOutboxWorker::start(
        &tools,
        AuthorizationSettings {
            deployment: deployment.clone(),
            outbox_poll_interval_ms: 10,
            outbox_batch_size: 100,
            outbox_lease_seconds: 5,
            outbox_max_retry_seconds: 5,
        },
    )
    .await?;

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let username = format!("integration_{suffix}");
    let password = "correct-horse-battery-staple";
    let registered = data(
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
    let user_id = registered["id"].as_i64().context("注册响应缺少用户 id")?;

    let login_response = dispatch(
        &application.runtime,
        "account.user",
        "login",
        json!({ "username": username, "password": password }),
        &[],
        &[],
    )
    .await?;
    let refresh_token = refresh_cookie(&login_response)?;
    let login = data(login_response)?;
    let access_token = login["access_token"]
        .as_str()
        .context("登录响应缺少 access_token")?;
    let login_authz_version = token_authz_version(&tools, access_token)?;
    ensure!(
        login_authz_version == token_authz_version(&tools, &refresh_token)?,
        "同次登录签发的 Access/Refresh Token 必须携带同一授权版本"
    );
    let bootstrap = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({
                "secret": bootstrap_secret.clone(),
                "name": "Integration Administrator",
                "position": "Owner"
            }),
            &[("authorization", &format!("Bearer {access_token}"))],
            &[],
        )
        .await?,
    )?;
    let bootstrap_admin_id = bootstrap["id"]
        .as_i64()
        .context("平台管理员初始化响应缺少 id")?;
    assert_authorization_error_with_body(
        &application.runtime,
        "admin.user",
        "bootstrap",
        access_token,
        json!({ "secret": bootstrap_secret, "name": "Second Administrator" }),
        700002,
    )
    .await?;

    let refresh_cookie_header = format!("yang_refresh={refresh_token}");
    let refreshed_admin_response = dispatch(
        &application.runtime,
        "account.user",
        "refresh",
        json!({}),
        &[("cookie", refresh_cookie_header.as_str())],
        &[],
    )
    .await?;
    let admin_refresh_token = refresh_cookie(&refreshed_admin_response)?;
    let refreshed_admin = data(refreshed_admin_response)?;
    let admin_access_token = refreshed_admin["access_token"]
        .as_str()
        .context("平台管理员刷新响应缺少 access_token")?;
    ensure!(
        token_authz_version(&tools, admin_access_token)?
            == token_authz_version(&tools, &admin_refresh_token)?,
        "refresh 签发的 Access/Refresh Token 必须携带同一授权版本"
    );
    ensure!(
        token_authz_version(&tools, admin_access_token)? == login_authz_version + 1,
        "bootstrap 必须在同一事务中递增目标用户授权版本"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), user_id).await?
            == token_authz_version(&tools, admin_access_token)?,
        "bootstrap 提交后的数据库版本必须与刷新快照一致"
    );
    wait_for_cached_version(&authorization_cache_probe, user_id, login_authz_version + 1).await?;
    for (module, action) in [
        ("account.user", "ui_catalog"),
        ("admin.user", "list"),
        ("org.tenant", "list"),
        ("org.org", "list"),
        ("org.user", "select"),
    ] {
        assert_authorization_error(&application.runtime, module, action, access_token, 400009)
            .await?;
    }
    let admin_authz_version = login_authz_version + 1;
    let admin_cache_key = authorization_cache_key(&deployment, user_id);
    let admin_cache_keys = [admin_cache_key.clone()];

    redis.del(&admin_cache_keys).await?;
    assert_authorization_success(
        &application.runtime,
        "account.user",
        "ui_catalog",
        admin_access_token,
    )
    .await?;
    ensure!(
        authorization_cache_probe.read(user_id).await?
            == CachedAuthorizationVersion::Version(admin_authz_version),
        "缓存缺失时必须回源 MySQL 并回填当前版本"
    );

    redis.set(&admin_cache_key, "malformed").await?;
    assert_authorization_success(
        &application.runtime,
        "account.user",
        "ui_catalog",
        admin_access_token,
    )
    .await?;
    ensure!(
        authorization_cache_probe.read(user_id).await?
            == CachedAuthorizationVersion::Version(admin_authz_version),
        "缓存值损坏时必须回源 MySQL 并修复缓存"
    );

    redis
        .set(&admin_cache_key, login_authz_version.to_string())
        .await?;
    assert_authorization_success(
        &application.runtime,
        "account.user",
        "ui_catalog",
        admin_access_token,
    )
    .await?;
    ensure!(
        authorization_cache_probe.read(user_id).await?
            == CachedAuthorizationVersion::Version(admin_authz_version),
        "缓存落后时必须以 MySQL 事实版本为准并推进缓存"
    );

    redis
        .set(&admin_cache_key, (admin_authz_version + 1).to_string())
        .await?;
    assert_authorization_error(
        &application.runtime,
        "account.user",
        "ui_catalog",
        admin_access_token,
        400009,
    )
    .await?;

    redis.del(&admin_cache_keys).await?;
    redis
        .lpush(&admin_cache_key, &["wrong-type".to_string()])
        .await?;
    assert_authorization_success(
        &application.runtime,
        "account.user",
        "ui_catalog",
        admin_access_token,
    )
    .await?;
    redis.del(&admin_cache_keys).await?;
    authorization_cache_probe
        .publish(user_id, admin_authz_version)
        .await?;

    let user_id_subject = user_id.to_string();
    let future_version_token = tools.token()?.generate_access_token(
        &user_id_subject,
        json!({ "authz_version": admin_authz_version + 1 }),
    )?;
    assert_authorization_error(
        &application.runtime,
        "account.user",
        "ui_catalog",
        &future_version_token,
        400010,
    )
    .await?;
    let missing_version_token = tools
        .token()?
        .generate_access_token(&user_id_subject, json!({}))?;
    assert_authorization_error(
        &application.runtime,
        "account.user",
        "ui_catalog",
        &missing_version_token,
        400010,
    )
    .await?;
    let invalid_subject_token = tools.token()?.generate_access_token(
        "not-a-user-id",
        json!({ "authz_version": admin_authz_version }),
    )?;
    assert_authorization_error(
        &application.runtime,
        "account.user",
        "ui_catalog",
        &invalid_subject_token,
        400010,
    )
    .await?;

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

    let member_username = format!("member_{suffix}");
    let member = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({ "username": member_username.clone(), "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let member_id = member["id"].as_i64().context("成员注册响应缺少 id")?;
    let member_initial_version = database_authz_version(tools.mysql()?.pool(), member_id).await?;
    let platform_member = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "add",
            json!({
                "user_user": member_id,
                "name": "Integration Platform Member",
                "admin": false
            }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let platform_member_id = platform_member["id"]
        .as_i64()
        .context("添加平台账号响应缺少 id")?;
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == member_initial_version + 1,
        "新增平台账号必须递增目标用户授权版本"
    );
    let member_login = data(
        dispatch(
            &application.runtime,
            "account.user",
            "login",
            json!({ "username": member_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let member_platform_access = member_login["access_token"]
        .as_str()
        .context("平台成员登录响应缺少 access_token")?;

    let main_version_before_rejected_demotion =
        database_authz_version(tools.mysql()?.pool(), user_id).await?;
    ensure!(
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": bootstrap_admin_id, "admin": false }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await
        .is_err(),
        "最后一个启用中的超级管理员不得被降级"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), user_id).await?
            == main_version_before_rejected_demotion,
        "失败事务不得递增授权版本"
    );

    data(
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": platform_member_id, "admin": false }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let member_version_after_idempotent_admin =
        database_authz_version(tools.mysql()?.pool(), member_id).await?;
    ensure!(
        member_version_after_idempotent_admin == member_initial_version + 1,
        "幂等 admin 写不得递增授权版本"
    );
    let concurrent_admin_headers = [("authorization", admin_authorization.as_str())];
    let (set_admin_a, set_admin_b, set_admin_c, set_admin_d) = tokio::join!(
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": platform_member_id, "admin": true }),
            &concurrent_admin_headers,
            &[],
        ),
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": platform_member_id, "admin": true }),
            &concurrent_admin_headers,
            &[],
        ),
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": platform_member_id, "admin": true }),
            &concurrent_admin_headers,
            &[],
        ),
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": platform_member_id, "admin": true }),
            &concurrent_admin_headers,
            &[],
        ),
    );
    for response in [set_admin_a, set_admin_b, set_admin_c, set_admin_d] {
        data(response?)?;
    }
    data(
        dispatch(
            &application.runtime,
            "admin.user",
            "set_admin",
            json!({ "id": platform_member_id, "admin": false }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == member_version_after_idempotent_admin + 2,
        "并发幂等授予只能递增一次，随后撤销再递增一次"
    );
    wait_for_cached_version(
        &authorization_cache_probe,
        member_id,
        member_version_after_idempotent_admin + 2,
    )
    .await?;
    assert_authorization_error(
        &application.runtime,
        "admin.user",
        "list",
        member_platform_access,
        400009,
    )
    .await?;

    data(
        dispatch(
            &application.runtime,
            "admin.user",
            "set_status",
            json!({ "id": platform_member_id, "status": "active" }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let member_version_after_idempotent_status =
        database_authz_version(tools.mysql()?.pool(), member_id).await?;
    for status in ["disabled", "active"] {
        data(
            dispatch(
                &application.runtime,
                "admin.user",
                "set_status",
                json!({ "id": platform_member_id, "status": status }),
                &[("authorization", &admin_authorization)],
                &[],
            )
            .await?,
        )?;
    }
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == member_version_after_idempotent_status + 2,
        "平台账号停用与启用必须各递增一次授权版本"
    );
    let audit_count_before_forced_failure: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_event")
            .fetch_one(tools.mysql()?.pool())
            .await?;
    let outbox_count_before_forced_failure: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ?")
            .bind(member_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    let version_before_forced_failure =
        database_authz_version(tools.mysql()?.pool(), member_id).await?;
    sqlx::query("RENAME TABLE audit_event TO audit_event_unavailable")
        .execute(tools.mysql()?.pool())
        .await?;
    let forced_audit_failure = dispatch(
        &application.runtime,
        "admin.user",
        "set_status",
        json!({ "id": platform_member_id, "status": "disabled" }),
        &[("authorization", &admin_authorization)],
        &[],
    )
    .await;
    sqlx::query("RENAME TABLE audit_event_unavailable TO audit_event")
        .execute(tools.mysql()?.pool())
        .await?;
    ensure!(
        forced_audit_failure.is_err(),
        "审计事实无法追加时，高权限业务写必须失败"
    );
    let status_after_forced_failure: String =
        sqlx::query_scalar("SELECT status FROM admin_user WHERE id = ?")
            .bind(platform_member_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    ensure!(
        status_after_forced_failure == "active",
        "审计追加失败必须回滚平台账号状态"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == version_before_forced_failure,
        "审计追加失败必须回滚授权版本"
    );
    let outbox_count_after_forced_failure: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM authorization_outbox WHERE user_id = ?")
            .bind(member_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    ensure!(
        outbox_count_after_forced_failure == outbox_count_before_forced_failure,
        "审计追加失败必须回滚授权 Outbox"
    );
    let audit_count_after_forced_failure: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_event")
            .fetch_one(tools.mysql()?.pool())
            .await?;
    ensure!(
        audit_count_after_forced_failure == audit_count_before_forced_failure,
        "失败业务事务不得留下成功审计事件"
    );

    let creator_version_before_onboarding =
        database_authz_version(tools.mysql()?.pool(), user_id).await?;
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
    ensure!(
        database_authz_version(tools.mysql()?.pool(), user_id).await?
            == creator_version_before_onboarding + 1,
        "租户 onboarding 必须与创始管理员成员关系原子递增授权版本"
    );

    let tenant_id = organization_id.to_string();
    let member_body = json!({
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
    let admin_refresh_cookie = format!("yang_refresh={admin_refresh_token}");
    let refreshed_org_admin_response = dispatch(
        &application.runtime,
        "account.user",
        "refresh",
        json!({}),
        &[("cookie", admin_refresh_cookie.as_str())],
        &[],
    )
    .await?;
    let org_refresh_token = refresh_cookie(&refreshed_org_admin_response)?;
    let refreshed_org_admin = data(refreshed_org_admin_response)?;
    let org_access_token = refreshed_org_admin["access_token"]
        .as_str()
        .context("组织管理员刷新响应缺少 access_token")?;
    let authorization = format!("Bearer {org_access_token}");
    let member_version_before_org_add =
        database_authz_version(tools.mysql()?.pool(), member_id).await?;
    let membership = data(
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
    let membership_id = membership["id"]
        .as_i64()
        .context("新增企业成员响应缺少 id")?;
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == member_version_before_org_add + 1,
        "新增企业成员必须原子递增目标用户授权版本"
    );

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

    let member_version_after_add = database_authz_version(tools.mysql()?.pool(), member_id).await?;
    for data_patch in [
        json!({ "name": "Integration Member Renamed" }),
        json!({ "admin": false, "status": "active" }),
    ] {
        data(
            dispatch(
                &application.runtime,
                "org.user",
                "put",
                json!({ "id": membership_id, "data": data_patch }),
                &[
                    ("authorization", &authorization),
                    ("x-tenant-id", &tenant_id),
                ],
                &[],
            )
            .await?,
        )?;
    }
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await? == member_version_after_add,
        "展示字段与幂等授权写不得递增企业成员授权版本"
    );

    for data_patch in [
        json!({ "admin": true }),
        json!({ "admin": false }),
        json!({ "status": "disabled" }),
        json!({ "status": "active" }),
    ] {
        data(
            dispatch(
                &application.runtime,
                "org.user",
                "put",
                json!({ "id": membership_id, "data": data_patch }),
                &[
                    ("authorization", &authorization),
                    ("x-tenant-id", &tenant_id),
                ],
                &[],
            )
            .await?,
        )?;
    }
    let member_version_after_role_changes =
        database_authz_version(tools.mysql()?.pool(), member_id).await?;
    ensure!(
        member_version_after_role_changes == member_version_after_add + 4,
        "成员管理员与状态的四次有效迁移必须各递增一次授权版本"
    );

    let replacement_username = format!("replacement_{suffix}");
    let replacement = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({ "username": replacement_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let replacement_id = replacement["id"]
        .as_i64()
        .context("替换成员注册响应缺少 id")?;
    let replacement_initial_version =
        database_authz_version(tools.mysql()?.pool(), replacement_id).await?;
    data(
        dispatch(
            &application.runtime,
            "org.user",
            "put",
            json!({ "id": membership_id, "data": { "user_user": replacement_id } }),
            &[
                ("authorization", &authorization),
                ("x-tenant-id", &tenant_id),
            ],
            &[],
        )
        .await?,
    )?;
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == member_version_after_role_changes + 1,
        "成员绑定用户变化必须递增旧用户授权版本"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), replacement_id).await?
            == replacement_initial_version + 1,
        "成员绑定用户变化必须递增新用户授权版本"
    );

    data(
        dispatch(
            &application.runtime,
            "org.user",
            "del",
            json!({ "id": membership_id }),
            &[
                ("authorization", &authorization),
                ("x-tenant-id", &tenant_id),
            ],
            &[],
        )
        .await?,
    )?;
    ensure!(
        database_authz_version(tools.mysql()?.pool(), replacement_id).await?
            == replacement_initial_version + 2,
        "删除企业成员必须原子递增当前绑定用户授权版本"
    );
    let org_refresh_cookie = format!("yang_refresh={org_refresh_token}");
    let outage_tokens = data(
        dispatch(
            &application.runtime,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", org_refresh_cookie.as_str())],
            &[],
        )
        .await?,
    )?;
    let outage_admin_access_token = outage_tokens["access_token"]
        .as_str()
        .context("故障矩阵刷新响应缺少 access_token")?
        .to_owned();
    let inconsistent_outbox_users: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM (\
            SELECT u.id \
            FROM users u \
            LEFT JOIN authorization_outbox o ON o.user_id = u.id \
            WHERE u.authz_version > 1 \
            GROUP BY u.id, u.authz_version \
            HAVING COUNT(o.id) <> u.authz_version - 1 \
                OR MIN(o.authz_version) <> 2 \
                OR MAX(o.authz_version) <> u.authz_version\
        ) inconsistent",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        inconsistent_outbox_users == 0,
        "每次已提交授权版本递增都必须恰好产生连续、无重复的 Outbox 事件"
    );
    wait_for_outbox_idle(tools.mysql()?.pool()).await?;
    let invalid_outbox_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authorization_outbox \
         WHERE state <> 'published' \
            OR attempts < 1 OR available_at <= 0 OR created_at <= 0 \
            OR lease_until IS NOT NULL OR worker_id IS NOT NULL \
            OR last_error IS NOT NULL \
            OR published_at IS NULL",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        invalid_outbox_rows == 0,
        "Outbox 清空后只能留下已确认发布且租约已释放的事件"
    );
    let audit_action_counts: Vec<(String, i64)> =
        sqlx::query_as("SELECT action, COUNT(*) FROM audit_event GROUP BY action ORDER BY action")
            .fetch_all(tools.mysql()?.pool())
            .await?;
    let audit_action_count = |action: &str| {
        audit_action_counts
            .iter()
            .find_map(|(candidate, count)| (candidate == action).then_some(*count))
            .unwrap_or_default()
    };
    for (action, minimum) in [
        ("admin.user.bootstrap", 1),
        ("admin.user.add", 1),
        ("admin.user.set_admin", 2),
        ("admin.user.set_status", 2),
        ("org.tenant.create", 1),
        ("org.user.add", 1),
        ("org.user.put", 6),
        ("org.user.del", 1),
    ] {
        ensure!(
            audit_action_count(action) >= minimum,
            "已提交高权限写缺少审计事件: action={action}, counts={audit_action_counts:?}"
        );
    }
    let invalid_audit_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_event \
         WHERE actor_type <> 'user' OR result <> 'succeeded' \
            OR subject_type <> 'user' OR subject_id IS NULL \
            OR (before_summary IS NULL AND after_summary IS NULL) \
            OR (before_summary IS NOT NULL AND JSON_TYPE(before_summary) <> 'OBJECT') \
            OR (after_summary IS NOT NULL AND JSON_TYPE(after_summary) <> 'OBJECT')",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        invalid_audit_rows == 0,
        "高权限成功事件必须携带操作者、subject 和受控 JSON 摘要"
    );
    let (audit_rows, distinct_events, distinct_requests): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT event_id), COUNT(DISTINCT request_id) FROM audit_event",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        audit_rows == distinct_events && audit_rows == distinct_requests,
        "每个高权限事务必须产生唯一且可关联的审计事实"
    );
    outbox_worker.shutdown().await?;

    let redis_outage_authorization_redis = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(2)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await?;
    let redis_outage_cache = AuthorizationVersionCache::new(
        redis_outage_authorization_redis.clone(),
        format!("{deployment}-redis-down"),
    )?;
    redis_outage_authorization_redis.close().await;
    let redis_outage_database =
        Database::from_pool(tools.mysql()?.pool().clone(), database_config.clone())?;
    let redis_outage_tools = Arc::new(
        ToolsBuilder::new()
            .mysql(redis_outage_database)
            .cache(redis.clone())
            .extension(redis_outage_cache)
            .token(integration_token_manager())
            .build()?,
    );
    let redis_outage_app = build_app(redis_outage_tools, Arc::clone(&security))?;
    assert_authorization_success(
        &redis_outage_app.runtime,
        "account.user",
        "ui_catalog",
        &outage_admin_access_token,
    )
    .await?;

    let mysql_outage_authorization_redis = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(2)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await?;
    let mysql_outage_deployment = format!("{deployment}-mysql-down");
    let mysql_outage_cache = AuthorizationVersionCache::new(
        mysql_outage_authorization_redis.clone(),
        mysql_outage_deployment.clone(),
    )?;
    let current_admin_version = database_authz_version(tools.mysql()?.pool(), user_id).await?;
    ensure!(
        token_authz_version(&tools, &outage_admin_access_token)? == current_admin_version,
        "MySQL 故障矩阵必须使用当前授权版本 Token"
    );
    mysql_outage_cache
        .publish(user_id, current_admin_version)
        .await?;
    let mysql_outage_database =
        Database::from_pool(tools.mysql()?.pool().clone(), database_config)?;
    let mysql_outage_tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql_outage_database)
            .cache(redis.clone())
            .extension(mysql_outage_cache)
            .token(integration_token_manager())
            .build()?,
    );
    let mysql_outage_app = build_app(mysql_outage_tools, security)?;

    tools.mysql()?.close().await;
    assert_authorization_success(
        &mysql_outage_app.runtime,
        "account.user",
        "ui_catalog",
        &outage_admin_access_token,
    )
    .await?;
    let mysql_outage_key = authorization_cache_key(&mysql_outage_deployment, user_id);
    let mysql_outage_keys = [mysql_outage_key.clone()];
    mysql_outage_authorization_redis
        .set(&mysql_outage_key, (current_admin_version + 1).to_string())
        .await?;
    assert_authorization_error(
        &mysql_outage_app.runtime,
        "account.user",
        "ui_catalog",
        &outage_admin_access_token,
        400009,
    )
    .await?;
    mysql_outage_authorization_redis
        .del(&mysql_outage_keys)
        .await?;
    assert_authorization_error(
        &mysql_outage_app.runtime,
        "account.user",
        "ui_catalog",
        &outage_admin_access_token,
        400011,
    )
    .await?;
    mysql_outage_authorization_redis
        .set(&mysql_outage_key, "malformed")
        .await?;
    assert_authorization_error(
        &mysql_outage_app.runtime,
        "account.user",
        "ui_catalog",
        &outage_admin_access_token,
        400011,
    )
    .await?;
    mysql_outage_authorization_redis
        .del(&mysql_outage_keys)
        .await?;
    mysql_outage_authorization_redis
        .lpush(&mysql_outage_key, &["wrong-type".to_string()])
        .await?;
    assert_authorization_error(
        &mysql_outage_app.runtime,
        "account.user",
        "ui_catalog",
        &outage_admin_access_token,
        400011,
    )
    .await?;
    mysql_outage_authorization_redis
        .del(&mysql_outage_keys)
        .await?;
    mysql_outage_authorization_redis.close().await;

    tools.close().await;
    Ok(())
}
