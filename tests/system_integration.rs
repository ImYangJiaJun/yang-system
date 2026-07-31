use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use sqlx::{MySql, QueryBuilder};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::database::DatabaseInitializer;
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::{
    AuthorizationOutboxWorker, AuthorizationVersionCache, CachedAuthorizationVersion,
    ResourceAuthorizationCheckpoint, ResourceAuthorizationProbe,
};
use yang_system::bootstrap_secret::{generate_bootstrap_secret, BootstrapSecretVerifier};
use yang_system::config::{AuthorizationSettings, SecuritySettings};

const INTEGRATION_PASSWORD: &str = "correct-horse-battery-staple";

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

fn integration_step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new_with_keyring(
            "integration-step-up-active",
            "independent-integration-step-up-secret-32-bytes",
            std::iter::empty::<(&str, &str)>(),
            "yang-system-integration-step-up",
            "yang-system-integration-sensitive-actions",
        )
        .unwrap_or_else(|error| panic!("集成测试 Step-up keyring 应构建成功: {error}")),
    )
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
    dispatch_raw_with_step_up(app, module, action, body, headers, query)
        .await
        .map_err(|error| anyhow::anyhow!("{module}.{action} 调用失败: {error}"))
}

fn dispatch_raw_with_step_up<'a>(
    app: &'a BuiltApp,
    module: &'a str,
    action: &'a str,
    body: Value,
    headers: &'a [(&'a str, &'a str)],
    query: &'a [(&'a str, &'a str)],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ApiResponse, BaseError>> + 'a>> {
    Box::pin(async move {
        let first = dispatch_raw(app, module, action, body.clone(), headers, query).await;
        let challenge = match first {
            Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
            result => return result,
        };
        let authorization = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| *value)
            .ok_or_else(|| {
                BaseError::ConfigError("Step-up 目标请求缺少 authorization".to_string())
            })?;
        let token = authorization.strip_prefix("Bearer ").ok_or_else(|| {
            BaseError::ConfigError("Step-up 目标请求 authorization 不是 Bearer".to_string())
        })?;
        let claims = app.tools().token()?.verify_token(token)?;
        let username = claims
            .custom
            .get("username")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BaseError::ConfigError("Step-up Access Token 缺少 username".to_string())
            })?;
        let completed = dispatch_raw(
            app,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": challenge,
                "credentials": { "username": username, "password": INTEGRATION_PASSWORD }
            }),
            &[("authorization", authorization)],
            &[],
        )
        .await?;
        if completed.code != 0 {
            return Err(BaseError::ConfigError(format!(
                "集成测试 Step-up 完成失败: code={}",
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
        dispatch_raw(app, module, action, body, &retry_headers, query).await
    })
}

async fn dispatch_raw(
    app: &BuiltApp,
    module: &str,
    action: &str,
    body: Value,
    headers: &[(&str, &str)],
    query: &[(&str, &str)],
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    for (name, value) in query {
        request = request.query(*name, *value);
    }
    let peer: SocketAddr = "127.0.0.1:41000"
        .parse()
        .map_err(|error| BaseError::ConfigError(format!("测试对端地址无效: {error}")))?;
    let context = app
        .context(request)
        .with_request_meta(RequestMeta::new().with_peer_addr(peer));
    let handle = action_handle(app, module, action)
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    app.dispatch_context(handle, context).await
}

struct StepUpRequest<'a> {
    module: &'a str,
    action: &'a str,
    body: Value,
    authorization: &'a str,
    target_headers: Vec<(&'a str, &'a str)>,
    username: &'a str,
    password: &'a str,
}

async fn acquire_step_up_proof(
    app: &BuiltApp,
    request: StepUpRequest<'_>,
) -> anyhow::Result<String> {
    let mut headers = vec![("authorization", request.authorization)];
    headers.extend(request.target_headers);
    let challenge = match dispatch_raw(
        app,
        request.module,
        request.action,
        request.body,
        &headers,
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        Err(error) => anyhow::bail!(
            "{}.{} 获取 Step-up challenge 失败: {error}",
            request.module,
            request.action
        ),
        Ok(_) => anyhow::bail!(
            "{}.{} 缺少 proof 时不得执行",
            request.module,
            request.action
        ),
    };
    let completed = data(
        dispatch(
            app,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": challenge,
                "credentials": {
                    "username": request.username,
                    "password": request.password
                }
            }),
            &[("authorization", request.authorization)],
            &[],
        )
        .await?,
    )?;
    completed["proof"]
        .as_str()
        .map(ToOwned::to_owned)
        .context("Step-up 完成响应缺少 proof")
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

fn token_credential_version(tools: &yang_base::tools::Tools, token: &str) -> anyhow::Result<i64> {
    tools
        .token()?
        .verify_token(token)?
        .custom
        .get("credential_version")
        .and_then(Value::as_i64)
        .filter(|version| *version >= 0)
        .context("Refresh Token 缺少非负 credential_version")
}

async fn database_authz_version(pool: &sqlx::MySqlPool, user_id: i64) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT authz_version FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

async fn database_credential_version(pool: &sqlx::MySqlPool, user_id: i64) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT credential_version FROM users WHERE id = ?")
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

async fn reset_test_redis(redis: &RedisClient) -> anyhow::Result<()> {
    let keys = redis.keys("*").await?;
    if !keys.is_empty() {
        redis.del(&keys).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn step_up_is_audited_once_across_instances_and_fails_closed_without_redis(
) -> anyhow::Result<()> {
    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "Step-up 集成测试 Redis URL 必须使用独立 DB 15"
    );
    let database_config = DatabaseConfig::default()
        .with_max_connections(8)
        .with_min_connections(0)
        .with_connect_timeout(10);
    let mysql = Database::connect_with_config(&mysql_url, database_config.clone()).await?;
    reset_test_database(mysql.pool()).await?;
    let initializer_database = Database::from_pool(mysql.pool().clone(), database_config)?;
    let redis = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(8)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await?;
    reset_test_redis(&redis).await?;
    let deployment = format!(
        "step-up-integration-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let bootstrap = generate_bootstrap_secret()?;
    let bootstrap_secret = bootstrap.secret().to_owned();
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis.clone())
            .extension(AuthorizationVersionCache::new(redis.clone(), deployment)?)
            .extension(integration_step_up_manager())
            .token(integration_token_manager())
            .config(BootstrapSecretVerifier::new(bootstrap.digest().clone(), 2)?)
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 100,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: true,
        trusted_proxy_cidrs: Vec::new(),
    });
    let first = build_app(Arc::clone(&tools), Arc::clone(&security))?;
    let initializer = DatabaseInitializer::new(initializer_database, false);
    initializer
        .sync_table_definitions(&first.runtime.table_definitions().iter().collect::<Vec<_>>())
        .await?;
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
    sqlx::raw_sql(include_str!(
        "../migrations/20260731_0011_create_password_reset_token.sql"
    ))
    .execute(tools.mysql()?.pool())
    .await?;
    let second = build_app(Arc::clone(&tools), security)?;

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let admin_username = format!("step_up_admin_{suffix}");
    let target_username = format!("step_up_target_{suffix}");
    let password = "correct-horse-battery-staple";
    let admin = data(
        dispatch(
            &first.runtime,
            "account.user",
            "register",
            json!({ "username": admin_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let admin_id = admin["id"].as_i64().context("Step-up 管理员缺少 ID")?;
    let target = data(
        dispatch(
            &first.runtime,
            "account.user",
            "register",
            json!({ "username": target_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let target_id = target["id"].as_i64().context("Step-up 目标用户缺少 ID")?;
    let initial_login = data(
        dispatch(
            &first.runtime,
            "account.user",
            "login",
            json!({ "username": admin_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let initial_access = initial_login["access_token"]
        .as_str()
        .context("Step-up 初始登录缺少 Access Token")?;
    data(
        dispatch(
            &first.runtime,
            "admin.user",
            "bootstrap",
            json!({
                "secret": bootstrap_secret,
                "name": "Step-up Administrator",
                "position": "Owner"
            }),
            &[("authorization", &format!("Bearer {initial_access}"))],
            &[],
        )
        .await?,
    )?;
    let admin_login_response = dispatch(
        &first.runtime,
        "account.user",
        "login",
        json!({ "username": admin_username, "password": password }),
        &[],
        &[],
    )
    .await?;
    let admin_login = data(admin_login_response)?;
    let access_token = admin_login["access_token"]
        .as_str()
        .context("Step-up 管理员登录缺少 Access Token")?
        .to_owned();
    let authorization = format!("Bearer {access_token}");
    let body = json!({
        "user_user": target_id,
        "name": "Step-up Target",
        "admin": false
    });
    let challenge = match dispatch_raw(
        &first.runtime,
        "admin.user",
        "add",
        body.clone(),
        &[("authorization", authorization.as_str())],
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        Err(error) => anyhow::bail!("缺少 proof 应返回 StepUpRequired，实际: {error}"),
        Ok(_) => anyhow::bail!("缺少 proof 不得执行平台用户新增"),
    };
    let completed = data(
        dispatch(
            &first.runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": challenge,
                "credentials": { "username": admin_username, "password": password }
            }),
            &[("authorization", authorization.as_str())],
            &[],
        )
        .await?,
    )?;
    let proof = completed["proof"]
        .as_str()
        .context("Step-up 完成响应缺少 proof")?
        .to_owned();
    let proof_headers = [
        ("authorization", authorization.as_str()),
        ("x-step-up-proof", proof.as_str()),
    ];
    let (first_result, second_result) = tokio::join!(
        dispatch_raw(
            &first.runtime,
            "admin.user",
            "add",
            body.clone(),
            &proof_headers,
            &[],
        ),
        dispatch_raw(
            &second.runtime,
            "admin.user",
            "add",
            body.clone(),
            &proof_headers,
            &[],
        )
    );
    let outcomes = [first_result, second_result];
    let outcome_codes = outcomes
        .iter()
        .map(|result| match result {
            Ok(response) => format!("ok:{}", response.code),
            Err(error) => format!("error:{}", error.code()),
        })
        .collect::<Vec<_>>();
    ensure!(
        outcomes.iter().filter(|result| result.is_ok()).count() == 1,
        "两个实例并发消费同一 proof 必须恰好一个成功: {outcome_codes:?}"
    );
    ensure!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(BaseError::Unauthorized(_))))
            .count()
            == 1,
        "另一个实例必须把同一 proof 作为重放拒绝"
    );

    let target_membership_id: i64 =
        sqlx::query_scalar("SELECT id FROM admin_user WHERE user_user = ?")
            .bind(target_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    let promote_body = json!({ "id": target_membership_id, "admin": true });
    let promote_challenge = match dispatch_raw(
        &first.runtime,
        "admin.user",
        "set_admin",
        promote_body.clone(),
        &[("authorization", authorization.as_str())],
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        Err(error) => anyhow::bail!("提升备用管理员应先返回 challenge，实际: {error}"),
        Ok(_) => anyhow::bail!("缺少 proof 不得提升备用管理员"),
    };
    let promote_completed = data(
        dispatch(
            &first.runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": promote_challenge,
                "credentials": { "username": admin_username, "password": password }
            }),
            &[("authorization", authorization.as_str())],
            &[],
        )
        .await?,
    )?;
    let promote_proof = promote_completed["proof"]
        .as_str()
        .context("备用管理员提升 proof 缺失")?;
    data(
        dispatch(
            &first.runtime,
            "admin.user",
            "set_admin",
            promote_body,
            &[
                ("authorization", authorization.as_str()),
                ("x-step-up-proof", promote_proof),
            ],
            &[],
        )
        .await?,
    )?;

    let organization = data(
        dispatch(
            &first.runtime,
            "org.tenant",
            "create",
            json!({
                "name": "Self-disable adversarial organization",
                "code": format!("SD{suffix}")
            }),
            &[("authorization", authorization.as_str())],
            &[],
        )
        .await?,
    )?;
    let organization_id = organization["id"]
        .as_i64()
        .context("自助停用对抗企业缺少 ID")?;
    let tenant_id = organization_id.to_string();

    // onboarding 递增了管理员授权版本，重新登录获取包含企业身份的新快照。
    let lifecycle_login_response = dispatch(
        &first.runtime,
        "account.user",
        "login",
        json!({ "username": admin_username, "password": password }),
        &[],
        &[],
    )
    .await?;
    let admin_refresh = refresh_cookie(&lifecycle_login_response)?;
    let lifecycle_login = data(lifecycle_login_response)?;
    let lifecycle_access = lifecycle_login["access_token"]
        .as_str()
        .context("账号生命周期登录缺少 Access Token")?
        .to_owned();
    let authorization = format!("Bearer {lifecycle_access}");

    let authz_before_rejected_disable =
        database_authz_version(tools.mysql()?.pool(), admin_id).await?;
    let credential_before_rejected_disable =
        database_credential_version(tools.mysql()?.pool(), admin_id).await?;
    let rejected_disable_proof = acquire_step_up_proof(
        &first.runtime,
        StepUpRequest {
            module: "account.user",
            action: "disable_self",
            body: json!({}),
            authorization: authorization.as_str(),
            target_headers: Vec::new(),
            username: admin_username.as_str(),
            password,
        },
    )
    .await?;
    let rejected_disable = dispatch_raw(
        &first.runtime,
        "account.user",
        "disable_self",
        json!({}),
        &[
            ("authorization", authorization.as_str()),
            ("x-step-up-proof", rejected_disable_proof.as_str()),
        ],
        &[],
    )
    .await;
    ensure!(
        matches!(rejected_disable, Err(BaseError::PermissionDenied(_))),
        "企业唯一管理员不得停用自身: {rejected_disable:?}"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), admin_id).await?
            == authz_before_rejected_disable
            && database_credential_version(tools.mysql()?.pool(), admin_id).await?
                == credential_before_rejected_disable,
        "最后企业管理员保护失败时不得留下版本副作用"
    );

    let target_org_body = json!({
        "user_user": target_id,
        "name": "Self-disable backup administrator",
        "admin": true,
        "status": "active"
    });
    let target_org_proof = acquire_step_up_proof(
        &first.runtime,
        StepUpRequest {
            module: "org.user",
            action: "add",
            body: target_org_body.clone(),
            authorization: authorization.as_str(),
            target_headers: vec![("x-tenant-id", tenant_id.as_str())],
            username: admin_username.as_str(),
            password,
        },
    )
    .await?;
    data(
        dispatch(
            &first.runtime,
            "org.user",
            "add",
            target_org_body,
            &[
                ("authorization", authorization.as_str()),
                ("x-tenant-id", tenant_id.as_str()),
                ("x-step-up-proof", target_org_proof.as_str()),
            ],
            &[],
        )
        .await?,
    )?;

    let recovery_login = data(
        dispatch(
            &first.runtime,
            "account.user",
            "login",
            json!({ "username": target_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let recovery_access = recovery_login["access_token"]
        .as_str()
        .context("备用管理员登录缺少 Access Token")?;
    let recovery_authorization = format!("Bearer {recovery_access}");

    let authz_before_logout = database_authz_version(tools.mysql()?.pool(), admin_id).await?;
    let credential_before_logout =
        database_credential_version(tools.mysql()?.pool(), admin_id).await?;
    let logout_challenge = match dispatch_raw(
        &first.runtime,
        "account.user",
        "logout",
        json!({}),
        &[("authorization", authorization.as_str())],
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        Err(error) => anyhow::bail!("全量会话撤销应先返回 challenge，实际: {error}"),
        Ok(_) => anyhow::bail!("缺少 proof 不得撤销全部会话"),
    };
    let logout_completed = data(
        dispatch(
            &first.runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": logout_challenge,
                "credentials": { "username": admin_username, "password": password }
            }),
            &[("authorization", authorization.as_str())],
            &[],
        )
        .await?,
    )?;
    let logout_proof = logout_completed["proof"]
        .as_str()
        .context("全量会话撤销 proof 缺失")?;
    let logout = data(
        dispatch(
            &first.runtime,
            "account.user",
            "logout",
            json!({}),
            &[
                ("authorization", authorization.as_str()),
                ("x-step-up-proof", logout_proof),
            ],
            &[],
        )
        .await?,
    )?;
    ensure!(
        logout["revoked_all_sessions"] == true
            && logout["immediate_convergence"] == true
            && logout["relogin_required"] == true,
        "全量会话撤销响应必须明确持久撤销、即时收敛和重新登录语义"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), admin_id).await? == authz_before_logout + 1,
        "全量会话撤销必须递增授权版本"
    );
    ensure!(
        database_credential_version(tools.mysql()?.pool(), admin_id).await?
            == credential_before_logout + 1,
        "全量会话撤销必须递增凭据版本"
    );
    ensure!(
        dispatch_raw(
            &first.runtime,
            "account.user",
            "me",
            json!({}),
            &[("authorization", authorization.as_str())],
            &[],
        )
        .await
        .is_err(),
        "全量撤销后旧 Access Token 必须失败"
    );
    let stale_refresh_cookie = format!("yang_refresh={admin_refresh}");
    ensure!(
        dispatch_raw(
            &first.runtime,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", stale_refresh_cookie.as_str())],
            &[],
        )
        .await
        .is_err(),
        "全量撤销后旧 Refresh Token 必须失败"
    );

    let disable_login_response = dispatch(
        &first.runtime,
        "account.user",
        "login",
        json!({ "username": admin_username, "password": password }),
        &[],
        &[],
    )
    .await?;
    let disable_refresh = refresh_cookie(&disable_login_response)?;
    let disable_login = data(disable_login_response)?;
    let disable_access = disable_login["access_token"]
        .as_str()
        .context("自助停用登录缺少 Access Token")?
        .to_owned();
    let disable_authorization = format!("Bearer {disable_access}");
    let authz_before_disable = database_authz_version(tools.mysql()?.pool(), admin_id).await?;
    let credential_before_disable =
        database_credential_version(tools.mysql()?.pool(), admin_id).await?;
    let disable_proof = acquire_step_up_proof(
        &first.runtime,
        StepUpRequest {
            module: "account.user",
            action: "disable_self",
            body: json!({}),
            authorization: disable_authorization.as_str(),
            target_headers: Vec::new(),
            username: admin_username.as_str(),
            password,
        },
    )
    .await?;
    let disabled = data(
        dispatch(
            &first.runtime,
            "account.user",
            "disable_self",
            json!({}),
            &[
                ("authorization", disable_authorization.as_str()),
                ("x-step-up-proof", disable_proof.as_str()),
            ],
            &[],
        )
        .await?,
    )?;
    ensure!(
        disabled["account_disabled"] == true
            && disabled["immediate_convergence"] == true
            && disabled["relogin_required"] == true,
        "自助停用响应必须明确账号停用、即时收敛和重新登录语义"
    );
    let disabled_state: (String, i64, i64) =
        sqlx::query_as("SELECT status, authz_version, credential_version FROM users WHERE id = ?")
            .bind(admin_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    ensure!(
        disabled_state
            == (
                "disabled".to_string(),
                authz_before_disable + 1,
                credential_before_disable + 1,
            ),
        "自助停用必须原子写入用户状态并各递增一次安全版本"
    );
    let disabled_platform_relations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_user WHERE user_user = ? AND status = 'disabled'",
    )
    .bind(admin_id)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    let disabled_org_relations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM org_user WHERE user_user = ? AND status = 'disabled'",
    )
    .bind(admin_id)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    let backup_platform_admins: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_user \
         WHERE user_user = ? AND status = 'active' AND admin = TRUE",
    )
    .bind(target_id)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    let backup_org_admins: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM org_user \
         WHERE user_user = ? AND org_org = ? AND status = 'active' AND admin = TRUE",
    )
    .bind(target_id)
    .bind(organization_id)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        disabled_platform_relations == 1
            && disabled_org_relations == 1
            && backup_platform_admins == 1
            && backup_org_admins == 1,
        "自助停用必须只停用当前用户关系，并保留平台/企业备用管理员"
    );
    ensure!(
        dispatch_raw(
            &first.runtime,
            "account.user",
            "me",
            json!({}),
            &[("authorization", disable_authorization.as_str())],
            &[],
        )
        .await
        .is_err(),
        "自助停用后旧 Access Token 必须失败"
    );
    let disabled_refresh_cookie = format!("yang_refresh={disable_refresh}");
    ensure!(
        dispatch_raw(
            &first.runtime,
            "account.user",
            "refresh",
            json!({}),
            &[("cookie", disabled_refresh_cookie.as_str())],
            &[],
        )
        .await
        .is_err(),
        "自助停用后旧 Refresh Token 必须失败"
    );

    let failed_body = json!({ "id": target_membership_id, "status": "active" });
    let failed_challenge = match dispatch_raw(
        &first.runtime,
        "admin.user",
        "set_status",
        failed_body.clone(),
        &[("authorization", recovery_authorization.as_str())],
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        Err(error) => anyhow::bail!("数据库故障前应获得 challenge，实际: {error}"),
        Ok(_) => anyhow::bail!("缺少 proof 不得执行状态更新"),
    };
    let failed_completed = data(
        dispatch(
            &first.runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": failed_challenge,
                "credentials": { "username": target_username, "password": password }
            }),
            &[("authorization", recovery_authorization.as_str())],
            &[],
        )
        .await?,
    )?;
    let failed_proof = failed_completed["proof"]
        .as_str()
        .context("数据库故障 proof 缺失")?
        .to_owned();
    sqlx::query("RENAME TABLE admin_user TO admin_user_unavailable")
        .execute(tools.mysql()?.pool())
        .await?;
    let failed_result = dispatch_raw(
        &first.runtime,
        "admin.user",
        "set_status",
        failed_body.clone(),
        &[
            ("authorization", recovery_authorization.as_str()),
            ("x-step-up-proof", failed_proof.as_str()),
        ],
        &[],
    )
    .await;
    sqlx::query("RENAME TABLE admin_user_unavailable TO admin_user")
        .execute(tools.mysql()?.pool())
        .await?;
    ensure!(
        failed_result.is_err(),
        "业务数据库故障时高危写入必须失败关闭"
    );

    let outage_challenge = match dispatch_raw(
        &first.runtime,
        "admin.user",
        "set_status",
        failed_body.clone(),
        &[("authorization", recovery_authorization.as_str())],
        &[],
    )
    .await
    {
        Err(BaseError::StepUpRequired(challenge)) => challenge.challenge,
        Err(error) => anyhow::bail!("Redis 故障前应获得 challenge，实际: {error}"),
        Ok(_) => anyhow::bail!("缺少 proof 不得执行状态更新"),
    };
    let outage_completed = data(
        dispatch(
            &first.runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": outage_challenge,
                "credentials": { "username": target_username, "password": password }
            }),
            &[("authorization", recovery_authorization.as_str())],
            &[],
        )
        .await?,
    )?;
    let outage_proof = outage_completed["proof"]
        .as_str()
        .context("Redis 故障 proof 缺失")?
        .to_owned();
    redis.close().await;
    let outage_result = dispatch_raw(
        &first.runtime,
        "admin.user",
        "set_status",
        failed_body,
        &[
            ("authorization", recovery_authorization.as_str()),
            ("x-step-up-proof", outage_proof.as_str()),
        ],
        &[],
    )
    .await;
    ensure!(
        outage_result.is_err(),
        "Redis proof store 不可用时高危写入必须失败关闭"
    );

    let audit_rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT action, result, target_type, CAST(after_summary AS CHAR) \
         FROM audit_event \
         WHERE action IN ('security.step_up', 'account.user.step_up_complete', \
                          'account.user.disable_self', 'admin.user.add', \
                          'admin.user.set_status') \
         ORDER BY id",
    )
    .fetch_all(tools.mysql()?.pool())
    .await?;
    ensure!(
        audit_rows.iter().any(|(action, result, _, summary)| {
            action == "security.step_up"
                && result == "succeeded"
                && summary
                    .as_deref()
                    .is_some_and(|value| value.contains("proof_accepted"))
        }),
        "proof 接受必须在业务 Action 前留下 succeeded 审计"
    );
    ensure!(
        audit_rows.iter().any(|(action, result, _, summary)| {
            action == "security.step_up"
                && result == "denied"
                && summary
                    .as_deref()
                    .is_some_and(|value| value.contains("proof_replayed"))
        }),
        "跨实例重放必须留下 denied 审计"
    );
    ensure!(
        audit_rows.iter().any(|(action, result, _, summary)| {
            action == "admin.user.set_status"
                && result == "failed"
                && summary
                    .as_deref()
                    .is_some_and(|value| value.contains("action_rejected"))
        }),
        "proof 接受后的业务故障必须留下独立 failed 审计"
    );
    ensure!(
        audit_rows.iter().any(|(action, result, target, summary)| {
            action == "account.user.disable_self"
                && result == "succeeded"
                && target == "user"
                && summary.as_deref().is_some_and(|value| {
                    value.contains("organization_relations_disabled")
                        && value.contains("platform_relations_disabled")
                })
        }),
        "自助停用必须留下关系数量去敏摘要的 succeeded 审计"
    );
    let serialized_audits = serde_json::to_string(&audit_rows)?;
    for sensitive in [
        admin_username.as_str(),
        target_username.as_str(),
        password,
        proof.as_str(),
        rejected_disable_proof.as_str(),
        target_org_proof.as_str(),
        disable_proof.as_str(),
        failed_proof.as_str(),
        outage_proof.as_str(),
    ] {
        ensure!(
            !serialized_audits.contains(sensitive),
            "Step-up 审计不得包含账号、密码或 proof"
        );
    }

    tools.close().await;
    Ok(())
}

async fn account_and_tenant_lifecycle_scenario() -> anyhow::Result<()> {
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
    reset_test_redis(&redis).await?;
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
            .extension(integration_step_up_manager())
            .token(integration_token_manager())
            .config(bootstrap_verifier)
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 100,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: true,
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
    sqlx::raw_sql(include_str!(
        "../migrations/20260731_0011_create_password_reset_token.sql"
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
        tools
            .token()?
            .verify_token(access_token)?
            .custom
            .get("credential_version")
            .is_none(),
        "Access Token 不得携带未被请求校验器消费的凭据版本"
    );
    ensure!(
        token_credential_version(&tools, &refresh_token)? == 0,
        "开启签发后 Refresh Token 必须携带数据库凭据版本"
    );

    let step_up_manager = tools.extension::<Arc<StepUpManager>>()?.clone();
    let step_up_action = ActionRef::new(
        ModuleName::new("admin.user")
            .map_err(|error| anyhow::anyhow!("Step-up ModuleName 无效: {error}"))?,
        ActionName::new("set_admin")
            .map_err(|error| anyhow::anyhow!("Step-up ActionName 无效: {error}"))?,
    );
    let step_up_resource = "admin_user:integration-target:admin=true";
    let step_up_challenge =
        step_up_manager.issue_challenge(user_id.to_string(), &step_up_action, step_up_resource)?;
    let signed_step_up_challenge = step_up_challenge.challenge.clone();
    let wrong_step_up = dispatch_raw(
        &application.runtime,
        "account.user",
        "step_up_complete",
        json!({
            "challenge": step_up_challenge.challenge.clone(),
            "credentials": { "username": username, "password": "wrong-step-up-password" }
        }),
        &[],
        &[],
    )
    .await;
    ensure!(
        matches!(wrong_step_up, Err(BaseError::InvalidPassword)),
        "错误 Step-up 密码必须返回统一凭据错误"
    );
    let failure_keys = [
        "yang-system:auth-failure:step-up-complete:ip:127.0.0.1".to_string(),
        format!("yang-system:auth-failure:step-up-complete:username:{username}"),
    ];
    for key in &failure_keys {
        ensure!(
            redis.get(key).await?.as_deref() == Some("1"),
            "Step-up 失败必须写入独立失败计数: {key}"
        );
    }
    let completed_step_up = data(
        dispatch(
            &application.runtime,
            "account.user",
            "step_up_complete",
            json!({
                "challenge": step_up_challenge.challenge,
                "credentials": { "username": username, "password": password }
            }),
            &[],
            &[],
        )
        .await?,
    )?;
    let proof = completed_step_up["proof"]
        .as_str()
        .context("Step-up 完成响应缺少 proof")?;
    step_up_manager.verify_proof(
        proof,
        &user_id.to_string(),
        &step_up_action,
        step_up_resource,
    )?;
    for key in &failure_keys {
        ensure!(
            redis.get(key).await?.is_none(),
            "正确 Step-up 密码必须清除当前身份的连续失败计数: {key}"
        );
    }
    let cross_subject = step_up_manager.issue_challenge(
        (user_id + 1).to_string(),
        &step_up_action,
        step_up_resource,
    )?;
    let cross_subject_attempt = dispatch_raw(
        &application.runtime,
        "account.user",
        "step_up_complete",
        json!({
            "challenge": cross_subject.challenge,
            "credentials": { "username": username, "password": password }
        }),
        &[],
        &[],
    )
    .await;
    ensure!(
        matches!(cross_subject_attempt, Err(BaseError::Unauthorized(_))),
        "正确密码也不得完成其他主体的 Step-up challenge"
    );
    let step_up_completion_audits: Vec<(String, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT result, actor_type, subject_id, CAST(after_summary AS CHAR) \
             FROM audit_event \
             WHERE action = 'account.user.step_up_complete' \
             ORDER BY id",
        )
        .fetch_all(tools.mysql()?.pool())
        .await?;
    ensure!(
        step_up_completion_audits.len() == 3,
        "错误密码、成功和跨主体尝试必须各持久化一条 Step-up 完成审计"
    );
    ensure!(
        step_up_completion_audits
            .iter()
            .filter(|(result, _, _, _)| result == "denied")
            .count()
            == 2,
        "错误密码与跨主体尝试必须记为 denied"
    );
    ensure!(
        step_up_completion_audits
            .iter()
            .filter(|(result, _, _, _)| result == "succeeded")
            .count()
            == 1,
        "正确凭据必须记为 succeeded"
    );
    let serialized_step_up_audits = serde_json::to_string(&step_up_completion_audits)?;
    for sensitive in [
        username.as_str(),
        password,
        "wrong-step-up-password",
        signed_step_up_challenge.as_str(),
        completed_step_up["proof"].as_str().unwrap_or_default(),
    ] {
        ensure!(
            !serialized_step_up_audits.contains(sensitive),
            "Step-up 持久审计不得包含用户名、密码、challenge 或 proof"
        );
    }

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
        token_credential_version(&tools, &admin_refresh_token)? == 0,
        "refresh 签发的新 Refresh Token 必须保持当前凭据版本"
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

    let reset_username = format!("password_reset_{suffix}");
    let reset_old_password = "reset-user-password-before";
    let reset_new_password_one = "reset-user-password-after-one";
    let reset_new_password_two = "reset-user-password-after-two";
    let reset_user = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({
                "username": reset_username.clone(),
                "password": reset_old_password
            }),
            &[],
            &[],
        )
        .await?,
    )?;
    let reset_user_id = reset_user["id"]
        .as_i64()
        .context("密码重置用户响应缺少 id")?;
    let reset_login_response = dispatch(
        &application.runtime,
        "account.user",
        "login",
        json!({
            "username": reset_username.clone(),
            "password": reset_old_password
        }),
        &[],
        &[],
    )
    .await?;
    let reset_old_refresh = refresh_cookie(&reset_login_response)?;
    let reset_login = data(reset_login_response)?;
    let reset_old_access = reset_login["access_token"]
        .as_str()
        .context("密码重置用户登录响应缺少 access_token")?
        .to_owned();
    assert_authorization_error_with_body(
        &application.runtime,
        "admin.user",
        "create_password_reset",
        &reset_old_access,
        json!({ "user_id": reset_user_id }),
        700002,
    )
    .await?;

    let first_reset_response = dispatch(
        &application.runtime,
        "admin.user",
        "create_password_reset",
        json!({ "user_id": reset_user_id }),
        &[("authorization", &admin_authorization)],
        &[],
    )
    .await?;
    ensure!(
        first_reset_response
            .response_headers()
            .iter()
            .any(|(name, value)| {
                name.eq_ignore_ascii_case("cache-control") && value == "no-store"
            }),
        "只返回一次的原始重置凭证响应必须禁止缓存"
    );
    let first_reset_created = data(first_reset_response)?;
    let first_reset_token = first_reset_created["reset_token"]
        .as_str()
        .context("首次创建响应缺少重置凭证")?
        .to_owned();
    let first_reset_fingerprint = first_reset_created["reset_fingerprint"]
        .as_str()
        .context("首次创建响应缺少重置指纹")?
        .to_owned();
    ensure!(
        first_reset_token.len() == 64
            && first_reset_fingerprint.len() == 16
            && first_reset_created.get("password").is_none(),
        "管理员响应只能返回一次性重置凭证与短指纹，不能返回临时密码"
    );
    let raw_token_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM password_reset_token \
         WHERE token_digest = ? OR token_fingerprint = ?",
    )
    .bind(&first_reset_token)
    .bind(&first_reset_token)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(raw_token_rows == 0, "原始重置凭证不得落库");
    let stored_fingerprint: String = sqlx::query_scalar(
        "SELECT CAST(token_fingerprint AS CHAR) FROM password_reset_token \
         WHERE user_user = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(reset_user_id)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        stored_fingerprint == first_reset_fingerprint,
        "数据库只应保存可关联的短指纹"
    );

    let second_reset_created = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "create_password_reset",
            json!({ "user_id": reset_user_id }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let second_reset_token = second_reset_created["reset_token"]
        .as_str()
        .context("第二次创建响应缺少重置凭证")?
        .to_owned();
    ensure!(
        first_reset_token != second_reset_token,
        "每次创建必须使用独立随机凭证"
    );
    let invalidated_prior: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM password_reset_token \
         WHERE user_user = ? AND token_fingerprint = ? AND invalidated_at IS NOT NULL",
    )
    .bind(reset_user_id)
    .bind(&first_reset_fingerprint)
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        invalidated_prior == 1,
        "创建新凭证必须原子失效旧的未消费凭证"
    );
    ensure!(
        matches!(
            dispatch_raw(
                &application.runtime,
                "account.user",
                "reset_password",
                json!({
                    "reset_token": first_reset_token,
                    "new_password": reset_new_password_one
                }),
                &[],
                &[],
            )
            .await,
            Err(BaseError::Unauthorized(_))
        ),
        "已被新凭证替换的旧凭证必须拒绝"
    );

    let reset_authz_before = database_authz_version(tools.mysql()?.pool(), reset_user_id).await?;
    let reset_credential_before =
        database_credential_version(tools.mysql()?.pool(), reset_user_id).await?;
    let reset_once = dispatch_raw(
        &application.runtime,
        "account.user",
        "reset_password",
        json!({
            "reset_token": second_reset_token.clone(),
            "new_password": reset_new_password_one
        }),
        &[],
        &[],
    );
    let reset_twice = dispatch_raw(
        &application.runtime,
        "account.user",
        "reset_password",
        json!({
            "reset_token": second_reset_token.clone(),
            "new_password": reset_new_password_two
        }),
        &[],
        &[],
    );
    let (reset_once_result, reset_twice_result) = tokio::join!(reset_once, reset_twice);
    ensure!(
        reset_once_result.is_ok() ^ reset_twice_result.is_ok(),
        "同一重置凭证并发消费必须恰好一次成功: first={reset_once_result:?}, second={reset_twice_result:?}"
    );
    let (reset_winning_password, reset_success, reset_loser) = match reset_once_result {
        Ok(response) => (
            reset_new_password_one,
            response,
            reset_twice_result.err().context("第二次消费应失败")?,
        ),
        Err(first_error) => (
            reset_new_password_two,
            reset_twice_result.context("第二次消费应成功")?,
            first_error,
        ),
    };
    ensure!(
        matches!(reset_loser, BaseError::Unauthorized(_)),
        "并发失败请求必须看到统一的无效凭证错误"
    );
    ensure!(
        reset_success
            .data
            .as_ref()
            .and_then(|value| value.get("relogin_required"))
            .and_then(Value::as_bool)
            == Some(true),
        "重置成功必须要求重新登录"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), reset_user_id).await?
            == reset_authz_before + 1
            && database_credential_version(tools.mysql()?.pool(), reset_user_id).await?
                == reset_credential_before + 1,
        "成功消费必须在同一事务中恰好递增两个版本"
    );
    ensure!(
        matches!(
            dispatch_raw(
                &application.runtime,
                "account.user",
                "reset_password",
                json!({
                    "reset_token": second_reset_token,
                    "new_password": "reset-user-password-after-reuse"
                }),
                &[],
                &[],
            )
            .await,
            Err(BaseError::Unauthorized(_))
        ),
        "成功消费后的同一凭证必须永久拒绝复用"
    );
    wait_for_cached_version(
        &authorization_cache_probe,
        reset_user_id,
        reset_authz_before + 1,
    )
    .await?;
    assert_authorization_error(
        &application.runtime,
        "account.user",
        "ui_catalog",
        &reset_old_access,
        400009,
    )
    .await?;
    let reset_old_cookie = format!("yang_refresh={reset_old_refresh}");
    ensure!(
        matches!(
            dispatch_raw(
                &application.runtime,
                "account.user",
                "refresh",
                json!({}),
                &[("cookie", reset_old_cookie.as_str())],
                &[],
            )
            .await,
            Err(BaseError::Unauthorized(_))
        ),
        "密码重置前的 Refresh Token 必须失效"
    );
    dispatch(
        &application.runtime,
        "account.user",
        "login",
        json!({
            "username": reset_username.clone(),
            "password": reset_winning_password
        }),
        &[],
        &[],
    )
    .await
    .context("重置后的新密码必须可登录")?;
    ensure!(
        dispatch_raw(
            &application.runtime,
            "account.user",
            "login",
            json!({
                "username": reset_username,
                "password": reset_old_password
            }),
            &[],
            &[],
        )
        .await
        .is_err(),
        "重置前的旧密码必须失效"
    );

    let expired_username = format!("reset_expired_{suffix}");
    let expired_user = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({
                "username": expired_username,
                "password": "reset-expired-password-before"
            }),
            &[],
            &[],
        )
        .await?,
    )?;
    let expired_user_id = expired_user["id"]
        .as_i64()
        .context("过期对抗用户响应缺少 id")?;
    let expired_reset = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "create_password_reset",
            json!({ "user_id": expired_user_id }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let expired_token = expired_reset["reset_token"]
        .as_str()
        .context("过期对抗响应缺少重置凭证")?;
    sqlx::query(
        "UPDATE password_reset_token \
         SET created_at = UNIX_TIMESTAMP() - 10, expires_at = UNIX_TIMESTAMP() - 1 \
         WHERE user_user = ? AND consumed_at IS NULL AND invalidated_at IS NULL",
    )
    .bind(expired_user_id)
    .execute(tools.mysql()?.pool())
    .await?;
    ensure!(
        matches!(
            dispatch_raw(
                &application.runtime,
                "account.user",
                "reset_password",
                json!({
                    "reset_token": expired_token,
                    "new_password": "reset-expired-password-after"
                }),
                &[],
                &[],
            )
            .await,
            Err(BaseError::Unauthorized(_))
        ),
        "到期凭证必须拒绝且不得消费"
    );

    let rollback_reset_username = format!("reset_rollback_{suffix}");
    let rollback_reset_user = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({
                "username": rollback_reset_username.clone(),
                "password": "reset-rollback-password-before"
            }),
            &[],
            &[],
        )
        .await?,
    )?;
    let rollback_reset_user_id = rollback_reset_user["id"]
        .as_i64()
        .context("重置回滚用户响应缺少 id")?;
    let rollback_reset = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "create_password_reset",
            json!({ "user_id": rollback_reset_user_id }),
            &[("authorization", &admin_authorization)],
            &[],
        )
        .await?,
    )?;
    let rollback_reset_token = rollback_reset["reset_token"]
        .as_str()
        .context("重置回滚响应缺少凭证")?
        .to_owned();
    let rollback_reset_authz =
        database_authz_version(tools.mysql()?.pool(), rollback_reset_user_id).await?;
    let rollback_reset_credential =
        database_credential_version(tools.mysql()?.pool(), rollback_reset_user_id).await?;
    sqlx::query("RENAME TABLE audit_event TO audit_event_unavailable")
        .execute(tools.mysql()?.pool())
        .await?;
    let reset_audit_failure = dispatch_raw(
        &application.runtime,
        "account.user",
        "reset_password",
        json!({
            "reset_token": rollback_reset_token.clone(),
            "new_password": "reset-rollback-password-after"
        }),
        &[],
        &[],
    )
    .await;
    let restore_audit = sqlx::query("RENAME TABLE audit_event_unavailable TO audit_event")
        .execute(tools.mysql()?.pool())
        .await;
    restore_audit.context("恢复重置回滚审计表失败")?;
    ensure!(reset_audit_failure.is_err(), "审计失败必须中止凭证消费");
    ensure!(
        database_authz_version(tools.mysql()?.pool(), rollback_reset_user_id).await?
            == rollback_reset_authz
            && database_credential_version(tools.mysql()?.pool(), rollback_reset_user_id).await?
                == rollback_reset_credential,
        "审计失败回滚后摘要、双版本和消费状态都必须保持不变"
    );
    dispatch(
        &application.runtime,
        "account.user",
        "reset_password",
        json!({
            "reset_token": rollback_reset_token,
            "new_password": "reset-rollback-password-after"
        }),
        &[],
        &[],
    )
    .await
    .context("审计恢复后同一未消费凭证必须仍可成功一次")?;

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

    sqlx::query("UPDATE admin_user SET admin = FALSE WHERE id = ?")
        .bind(bootstrap_admin_id)
        .execute(tools.mysql()?.pool())
        .await?;
    let stale_platform_write = dispatch_raw_with_step_up(
        &application.runtime,
        "admin.user",
        "set_status",
        json!({ "id": platform_member_id, "status": "active" }),
        &[("authorization", admin_authorization.as_str())],
        &[],
    )
    .await;
    sqlx::query("UPDATE admin_user SET admin = TRUE WHERE id = ?")
        .bind(bootstrap_admin_id)
        .execute(tools.mysql()?.pool())
        .await?;
    ensure!(
        matches!(stale_platform_write, Err(BaseError::PermissionDenied(_))),
        "平台权限快照仍有效但数据库管理员事实已撤销时，高危写必须在事务内失败"
    );

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

    let precheck_probe =
        ResourceAuthorizationProbe::new(ResourceAuthorizationCheckpoint::AfterPrecheck);
    let precheck_database =
        Database::from_pool(tools.mysql()?.pool().clone(), database_config.clone())?;
    let precheck_tools = Arc::new(
        ToolsBuilder::new()
            .mysql(precheck_database)
            .cache(redis.clone())
            .extension(authorization_cache_probe.clone())
            .extension(precheck_probe.clone())
            .extension(integration_step_up_manager())
            .token(integration_token_manager())
            .build()?,
    );
    let precheck_app = build_app(precheck_tools, Arc::clone(&security))?;
    let resource_headers = [
        ("authorization", authorization.as_str()),
        ("x-tenant-id", tenant_id.as_str()),
    ];
    let precheck_request = dispatch_raw_with_step_up(
        &precheck_app.runtime,
        "org.user",
        "put",
        json!({ "id": membership_id, "data": { "name": "must-not-commit" } }),
        &resource_headers,
        &[],
    );
    let revoke_before_linearization = async {
        precheck_probe.wait_until_reached().await;
        sqlx::query(
            "UPDATE org_user SET admin = FALSE \
             WHERE org_org = ? AND user_user = ?",
        )
        .bind(organization_id)
        .bind(user_id)
        .execute(tools.mysql()?.pool())
        .await?;
        precheck_probe.resume().await;
        Ok::<(), anyhow::Error>(())
    };
    let (precheck_attempt, revoke_result) =
        tokio::join!(precheck_request, revoke_before_linearization);
    revoke_result?;
    ensure!(
        matches!(precheck_attempt, Err(BaseError::PermissionDenied(_))),
        "middleware 预检后、事务线性化点前撤权必须拒绝写入"
    );
    let rejected_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM org_user WHERE id = ?")
            .bind(membership_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    ensure!(
        rejected_name.as_deref() != Some("must-not-commit"),
        "线性化点前撤权不得留下业务写入"
    );
    sqlx::query("UPDATE org_user SET admin = TRUE WHERE org_org = ? AND user_user = ?")
        .bind(organization_id)
        .bind(user_id)
        .execute(tools.mysql()?.pool())
        .await?;

    let linearized_probe =
        ResourceAuthorizationProbe::new(ResourceAuthorizationCheckpoint::AfterLinearization);
    let linearized_database =
        Database::from_pool(tools.mysql()?.pool().clone(), database_config.clone())?;
    let linearized_tools = Arc::new(
        ToolsBuilder::new()
            .mysql(linearized_database)
            .cache(redis.clone())
            .extension(authorization_cache_probe.clone())
            .extension(linearized_probe.clone())
            .extension(integration_step_up_manager())
            .token(integration_token_manager())
            .build()?,
    );
    let linearized_app = build_app(linearized_tools, Arc::clone(&security))?;
    let linearized_request = dispatch_raw_with_step_up(
        &linearized_app.runtime,
        "org.user",
        "put",
        json!({ "id": membership_id, "data": { "name": "linearized-write" } }),
        &resource_headers,
        &[],
    );
    let revoke_after_linearization = async {
        linearized_probe.wait_until_reached().await;
        let revoke = sqlx::query(
            "UPDATE org_user SET admin = FALSE \
             WHERE org_org = ? AND user_user = ?",
        )
        .bind(organization_id)
        .bind(user_id)
        .execute(tools.mysql()?.pool());
        tokio::pin!(revoke);
        tokio::select! {
            result = &mut revoke => {
                anyhow::bail!("授权事实行锁释放前撤权不应完成: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        linearized_probe.resume().await;
        revoke.await?;
        Ok::<(), anyhow::Error>(())
    };
    let (linearized_attempt, revoke_result) =
        tokio::join!(linearized_request, revoke_after_linearization);
    revoke_result?;
    data(linearized_attempt?)?;
    let committed_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM org_user WHERE id = ?")
            .bind(membership_id)
            .fetch_one(tools.mysql()?.pool())
            .await?;
    ensure!(
        committed_name.as_deref() == Some("linearized-write"),
        "线性化点后到达的撤权必须等待在途事务提交"
    );
    sqlx::query("UPDATE org_user SET admin = TRUE WHERE org_org = ? AND user_user = ?")
        .bind(organization_id)
        .bind(user_id)
        .execute(tools.mysql()?.pool())
        .await?;

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

    let change_username = format!("change_password_{suffix}");
    let change_password = "change-password-before";
    let first_replacement = "change-password-after-one";
    let second_replacement = "change-password-after-two";
    let change_user = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({ "username": change_username.clone(), "password": change_password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let change_user_id = change_user["id"]
        .as_i64()
        .context("改密对抗用户响应缺少 id")?;
    let change_login_response = dispatch(
        &application.runtime,
        "account.user",
        "login",
        json!({ "username": change_username.clone(), "password": change_password }),
        &[],
        &[],
    )
    .await?;
    let change_refresh_token = refresh_cookie(&change_login_response)?;
    let change_login = data(change_login_response)?;
    let change_access_token = change_login["access_token"]
        .as_str()
        .context("改密对抗登录响应缺少 access_token")?
        .to_owned();
    let initial_change_authz =
        database_authz_version(tools.mysql()?.pool(), change_user_id).await?;
    let initial_change_credential =
        database_credential_version(tools.mysql()?.pool(), change_user_id).await?;

    let wrong_old_password = dispatch_token_body_action(
        &application.runtime,
        "account.user",
        "change_password",
        &change_access_token,
        json!({
            "old_password": "definitely-not-the-current-password",
            "new_password": first_replacement
        }),
    )
    .await;
    ensure!(
        matches!(wrong_old_password, Err(BaseError::InvalidPassword)),
        "错误旧密码必须被拒绝且不能进入写事务"
    );
    let weak_new_password = dispatch_token_body_action(
        &application.runtime,
        "account.user",
        "change_password",
        &change_access_token,
        json!({ "old_password": change_password, "new_password": "short" }),
    )
    .await;
    ensure!(
        matches!(
            weak_new_password,
            Err(BaseError::ParamInvalid(field, _)) if field == "new_password"
        ),
        "弱新密码必须在进入 Argon2 和事务前被拒绝"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), change_user_id).await?
            == initial_change_authz
            && database_credential_version(tools.mysql()?.pool(), change_user_id).await?
                == initial_change_credential,
        "旧密码错误和弱新密码不得改变任何安全版本"
    );

    let first_change = dispatch_token_body_action(
        &application.runtime,
        "account.user",
        "change_password",
        &change_access_token,
        json!({ "old_password": change_password, "new_password": first_replacement }),
    );
    let second_change = dispatch_token_body_action(
        &application.runtime,
        "account.user",
        "change_password",
        &change_access_token,
        json!({ "old_password": change_password, "new_password": second_replacement }),
    );
    let (first_result, second_result) = tokio::join!(first_change, second_change);
    ensure!(
        first_result.is_ok() ^ second_result.is_ok(),
        "两个基于同一旧摘要的并发改密必须恰好一个成功: first={first_result:?}, second={second_result:?}"
    );
    let (winning_password, change_response, losing_error) = match first_result {
        Ok(response) => (
            first_replacement,
            response,
            second_result.err().context("第二个并发改密应失败")?,
        ),
        Err(first_error) => (
            second_replacement,
            second_result.context("第二个并发改密应成功")?,
            first_error,
        ),
    };
    ensure!(
        matches!(losing_error, BaseError::InvalidPassword)
            || matches!(
                losing_error,
                BaseError::ParamInvalid(ref field, _) if field == "old_password"
            ),
        "并发失败只能来自旧密码已经失效或持锁摘要复核冲突: {losing_error}"
    );
    ensure!(
        change_response
            .data
            .as_ref()
            .and_then(|value| value.get("relogin_required"))
            .and_then(Value::as_bool)
            == Some(true),
        "改密成功必须明确要求客户端重新登录"
    );
    ensure!(
        change_response
            .response_headers()
            .iter()
            .any(|(name, value)| {
                name.eq_ignore_ascii_case("set-cookie")
                    && value.contains("yang_refresh=;")
                    && value.contains("Max-Age=0")
            }),
        "改密成功必须清除浏览器 Refresh Cookie"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), change_user_id).await?
            == initial_change_authz + 1
            && database_credential_version(tools.mysql()?.pool(), change_user_id).await?
                == initial_change_credential + 1,
        "一次成功改密必须在同一事务中恰好递增两个版本"
    );
    wait_for_cached_version(
        &authorization_cache_probe,
        change_user_id,
        initial_change_authz + 1,
    )
    .await?;
    assert_authorization_error(
        &application.runtime,
        "account.user",
        "ui_catalog",
        &change_access_token,
        400009,
    )
    .await?;
    let stale_change_cookie = format!("yang_refresh={change_refresh_token}");
    ensure!(
        matches!(
            dispatch_raw(
                &application.runtime,
                "account.user",
                "refresh",
                json!({}),
                &[("cookie", stale_change_cookie.as_str())],
                &[],
            )
            .await,
            Err(BaseError::Unauthorized(_))
        ),
        "改密前签发且尚未过期的 Refresh Token 必须立即失效"
    );

    let changed_login_response = dispatch(
        &application.runtime,
        "account.user",
        "login",
        json!({ "username": change_username.clone(), "password": winning_password }),
        &[],
        &[],
    )
    .await?;
    let changed_login = data(changed_login_response)?;
    let changed_access_token = changed_login["access_token"]
        .as_str()
        .context("新密码登录响应缺少 access_token")?
        .to_owned();

    let unavailable_rate_cache = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(1)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await?;
    unavailable_rate_cache.close().await;
    let rate_outage_database =
        Database::from_pool(tools.mysql()?.pool().clone(), database_config.clone())?;
    let rate_outage_tools = Arc::new(
        ToolsBuilder::new()
            .mysql(rate_outage_database)
            .cache(unavailable_rate_cache)
            .extension(authorization_cache_probe.clone())
            .extension(integration_step_up_manager())
            .token(integration_token_manager())
            .build()?,
    );
    let rate_outage_app = build_app(rate_outage_tools, Arc::clone(&security))?;
    let before_rate_outage_authz =
        database_authz_version(tools.mysql()?.pool(), change_user_id).await?;
    let before_rate_outage_credential =
        database_credential_version(tools.mysql()?.pool(), change_user_id).await?;
    let rate_outage_attempt = dispatch_token_body_action(
        &rate_outage_app.runtime,
        "account.user",
        "change_password",
        &changed_access_token,
        json!({
            "old_password": winning_password,
            "new_password": "change-password-after-outage"
        }),
    )
    .await;
    ensure!(
        matches!(
            rate_outage_attempt,
            Err(BaseError::RedisConnectionFailed(_)) | Err(BaseError::RedisOperationFailed(_))
        ),
        "Redis 限流不可用时必须由 Redis 错误失败关闭"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), change_user_id).await?
            == before_rate_outage_authz
            && database_credential_version(tools.mysql()?.pool(), change_user_id).await?
                == before_rate_outage_credential,
        "Redis 失败不得改变密码相关版本"
    );

    let reset_rate_outage_attempt = dispatch_raw(
        &rate_outage_app.runtime,
        "account.user",
        "reset_password",
        json!({
            "reset_token": "0".repeat(64),
            "new_password": "reset-password-after-outage"
        }),
        &[],
        &[],
    )
    .await;
    ensure!(
        matches!(
            reset_rate_outage_attempt,
            Err(BaseError::RedisConnectionFailed(_)) | Err(BaseError::RedisOperationFailed(_))
        ),
        "Redis 限流不可用时公共密码重置入口必须失败关闭"
    );
    ensure!(
        database_authz_version(tools.mysql()?.pool(), change_user_id).await?
            == before_rate_outage_authz
            && database_credential_version(tools.mysql()?.pool(), change_user_id).await?
                == before_rate_outage_credential,
        "密码重置限流失败不得改变任何凭据版本"
    );

    let rollback_username = format!("change_rollback_{suffix}");
    let rollback_password = "rollback-password-before";
    let rollback_user = data(
        dispatch(
            &application.runtime,
            "account.user",
            "register",
            json!({ "username": rollback_username.clone(), "password": rollback_password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let rollback_user_id = rollback_user["id"]
        .as_i64()
        .context("回滚对抗用户响应缺少 id")?;
    let rollback_login = data(
        dispatch(
            &application.runtime,
            "account.user",
            "login",
            json!({ "username": rollback_username.clone(), "password": rollback_password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let rollback_access_token = rollback_login["access_token"]
        .as_str()
        .context("回滚对抗登录响应缺少 access_token")?
        .to_owned();
    let rollback_authz_before =
        database_authz_version(tools.mysql()?.pool(), rollback_user_id).await?;
    let rollback_credential_before =
        database_credential_version(tools.mysql()?.pool(), rollback_user_id).await?;
    sqlx::query("RENAME TABLE audit_event TO audit_event_unavailable")
        .execute(tools.mysql()?.pool())
        .await?;
    let rollback_attempt = dispatch_token_body_action(
        &application.runtime,
        "account.user",
        "change_password",
        &rollback_access_token,
        json!({
            "old_password": rollback_password,
            "new_password": "rollback-password-after"
        }),
    )
    .await;
    let restore_audit = sqlx::query("RENAME TABLE audit_event_unavailable TO audit_event")
        .execute(tools.mysql()?.pool())
        .await;
    restore_audit.context("恢复审计表失败")?;
    ensure!(rollback_attempt.is_err(), "审计写入失败必须中止改密事务");
    ensure!(
        database_authz_version(tools.mysql()?.pool(), rollback_user_id).await?
            == rollback_authz_before
            && database_credential_version(tools.mysql()?.pool(), rollback_user_id).await?
                == rollback_credential_before,
        "审计写失败回滚后两个版本必须保持不变"
    );
    dispatch(
        &application.runtime,
        "account.user",
        "login",
        json!({ "username": rollback_username, "password": rollback_password }),
        &[],
        &[],
    )
    .await
    .context("审计写失败回滚后旧密码仍应可登录")?;

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
        ("account.user.change_password", 1),
        ("account.user.reset_password", 2),
        ("admin.user.bootstrap", 1),
        ("admin.user.add", 1),
        ("admin.user.create_password_reset", 4),
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
         WHERE action IN ( \
               'account.user.change_password', 'account.user.reset_password', \
               'admin.user.bootstrap', 'admin.user.add', \
               'admin.user.create_password_reset', 'admin.user.set_admin', \
               'admin.user.set_status', 'org.tenant.create', \
               'org.user.add', 'org.user.put', 'org.user.del') \
           AND result = 'succeeded' \
           AND ((actor_type <> 'user' AND NOT (actor_type = 'system' AND action = 'account.user.reset_password')) \
             OR subject_type <> 'user' OR subject_id IS NULL \
             OR (before_summary IS NULL AND after_summary IS NULL) \
             OR (before_summary IS NOT NULL AND JSON_TYPE(before_summary) <> 'OBJECT') \
             OR (after_summary IS NOT NULL AND JSON_TYPE(after_summary) <> 'OBJECT'))",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        invalid_audit_rows == 0,
        "高权限成功事件必须携带操作者、subject 和受控 JSON 摘要"
    );
    let sensitive_change_audit_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_event \
         WHERE action = 'account.user.change_password' \
           AND LOWER(CONCAT( \
               COALESCE(CAST(before_summary AS CHAR), ''), \
               COALESCE(CAST(after_summary AS CHAR), '') \
           )) REGEXP 'password|token|cookie|secret|hash|credential|authorization'",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        sensitive_change_audit_rows == 0,
        "改密成功审计不得包含密码、Token、Cookie、摘要或版本敏感字段"
    );
    let sensitive_reset_audit_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_event \
         WHERE action IN ('admin.user.create_password_reset', 'account.user.reset_password') \
           AND LOWER(CONCAT( \
               COALESCE(CAST(before_summary AS CHAR), ''), \
               COALESCE(CAST(after_summary AS CHAR), '') \
           )) REGEXP 'password|token|cookie|secret|hash|credential|authorization'",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        sensitive_reset_audit_rows == 0,
        "密码重置审计只能记录短指纹和非敏感元数据"
    );
    let (audit_rows, distinct_events, distinct_requests): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT event_id), COUNT(DISTINCT request_id) FROM audit_event",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        audit_rows == distinct_events && distinct_requests <= audit_rows,
        "每条审计必须有唯一 event_id，并允许同一高危请求关联 proof 接受与业务结果"
    );
    let invalid_correlated_requests: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ( \
               SELECT request_id, COUNT(*) AS event_count, \
                      SUM(action = 'security.step_up' AND result = 'succeeded') AS proof_count, \
                      SUM(action <> 'security.step_up') AS business_count \
               FROM audit_event GROUP BY request_id HAVING COUNT(*) > 1 \
             ) AS correlated \
             WHERE event_count <> 2 OR proof_count <> 1 OR business_count <> 1",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        invalid_correlated_requests == 0,
        "重复 request_id 只能精确关联一条 Step-up 接受审计与一条业务结果审计"
    );

    let current_credential_version =
        database_credential_version(tools.mysql()?.pool(), user_id).await?;
    ensure!(
        current_credential_version == 0,
        "未发生凭据事件前版本应保持为 0"
    );
    let (_, future_refresh_token) = tools.token()?.generate_token_pair_with_refresh_claims(
        &user_id.to_string(),
        Value::Null,
        json!({ "credential_version": current_credential_version + 1 }),
    )?;
    let future_cookie = format!("yang_refresh={future_refresh_token}");
    let future_attempt = dispatch_raw(
        &application.runtime,
        "account.user",
        "refresh",
        json!({}),
        &[("cookie", future_cookie.as_str())],
        &[],
    )
    .await;
    ensure!(
        matches!(future_attempt, Err(BaseError::Unauthorized(_))),
        "领先数据库的 Refresh 凭据版本必须拒绝"
    );
    sqlx::query("UPDATE users SET credential_version = credential_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(tools.mysql()?.pool())
        .await?;
    let recovered_response = dispatch(
        &application.runtime,
        "account.user",
        "refresh",
        json!({}),
        &[("cookie", future_cookie.as_str())],
        &[],
    )
    .await?;
    let current_refresh_token = refresh_cookie(&recovered_response)?;
    ensure!(
        token_credential_version(&tools, &current_refresh_token)? == 1,
        "版本拒绝不得提前消费旧 JTI，数据库追平后同一 Token 应可完成一次轮换"
    );
    sqlx::query("UPDATE users SET credential_version = credential_version + 1 WHERE id = ?")
        .bind(user_id)
        .execute(tools.mysql()?.pool())
        .await?;
    let stale_cookie = format!("yang_refresh={current_refresh_token}");
    let stale_attempt = dispatch_raw(
        &application.runtime,
        "account.user",
        "refresh",
        json!({}),
        &[("cookie", stale_cookie.as_str())],
        &[],
    )
    .await;
    ensure!(
        matches!(stale_attempt, Err(BaseError::Unauthorized(_))),
        "落后数据库的旧 Refresh Token 即使未过期也必须拒绝"
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
            .extension(integration_step_up_manager())
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
            .extension(integration_step_up_manager())
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

#[test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
fn real_mysql_redis_support_account_and_tenant_lifecycle() -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("account-tenant-lifecycle".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("构建账户/租户生命周期测试运行时失败")?
                .block_on(account_and_tenant_lifecycle_scenario())
        })
        .context("创建账户/租户生命周期专用测试线程失败")?
        .join()
        .map_err(|_| anyhow::anyhow!("账户/租户生命周期专用测试线程 panic"))?
}

fn p95_millis(samples: &mut [u128]) -> u128 {
    samples.sort_unstable();
    let rank = (samples.len() * 95).div_ceil(100);
    samples[rank.saturating_sub(1)]
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL；会写入 1 万项目和 5 万任务"]
async fn work_addon_scale_and_adversarial_boundaries_hold() -> anyhow::Result<()> {
    const PROJECTS: usize = 10_000;
    const TASKS: usize = 50_000;
    const TREE_NODES: usize = 100;
    const SAMPLES: usize = 20;

    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "规模测试 Redis URL 必须使用独立 DB 15"
    );
    let database_config = DatabaseConfig::default()
        .with_max_connections(16)
        .with_min_connections(0)
        .with_connect_timeout(10);
    let mysql = Database::connect_with_config(&mysql_url, database_config.clone())
        .await
        .context("连接规模测试 MySQL 失败")?;
    reset_test_database(mysql.pool()).await?;
    let initializer_database = Database::from_pool(mysql.pool().clone(), database_config)?;
    let redis = RedisClient::connect_with_config(
        &redis_url,
        RedisConfig::default()
            .with_max_connections(16)
            .with_min_connections(0)
            .with_connect_timeout(10),
    )
    .await
    .context("连接规模测试 Redis 失败")?;
    reset_test_redis(&redis).await?;
    let deployment = format!(
        "work-scale-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let authorization_cache = AuthorizationVersionCache::new(redis.clone(), deployment)?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis)
            .extension(authorization_cache)
            .extension(integration_step_up_manager())
            .token(integration_token_manager())
            .build()?,
    );
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 100,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: true,
        trusted_proxy_cidrs: Vec::new(),
    });
    let application = build_app(Arc::clone(&tools), security)?;
    let initializer = DatabaseInitializer::new(initializer_database, false);
    let definitions = application
        .runtime
        .table_definitions()
        .iter()
        .collect::<Vec<_>>();
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
    let runtime = Arc::new(application.runtime);

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let username = format!("work_scale_{suffix}");
    let password = "correct-horse-battery-staple";
    let registered = data(
        dispatch(
            &runtime,
            "account.user",
            "register",
            json!({ "username": username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let user_id = registered["id"].as_i64().context("规模用户缺少 id")?;
    let login = data(
        dispatch(
            &runtime,
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
        .context("规模用户登录缺少 access_token")?
        .to_owned();
    let authorization = format!("Bearer {access_token}");
    let headers = [("authorization", authorization.as_str())];
    let pool = tools.mysql()?.pool();

    let seed_started = Instant::now();
    for chunk_start in (0..PROJECTS).step_by(1_000) {
        let chunk_end = (chunk_start + 1_000).min(PROJECTS);
        let mut query = QueryBuilder::<MySql>::new(
            "INSERT INTO work_project (owner_user, name, status, created_at, updated_at) ",
        );
        query.push_values(chunk_start..chunk_end, |mut row, index| {
            row.push_bind(user_id)
                .push_bind(format!("项目-{index:05}"))
                .push_bind("active")
                .push_bind(i64::try_from(index).unwrap_or_default())
                .push_bind(i64::try_from(index).unwrap_or_default());
        });
        query.build().execute(pool).await?;
    }
    let project_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM work_project WHERE owner_user = ? ORDER BY id")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    ensure!(project_ids.len() == PROJECTS, "项目规模数据写入不完整");

    let tree_project = project_ids[0];
    let mut tree_ids = Vec::with_capacity(TREE_NODES);
    for index in 0..TREE_NODES {
        let parent = tree_ids.last().copied();
        let result = sqlx::query(
            "INSERT INTO work_task \
             (owner_user, project_project, parent_task, title, status, priority, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'todo', 'normal', ?, ?)",
        )
        .bind(user_id)
        .bind(tree_project)
        .bind(parent)
        .bind(format!("树任务-{index:03}"))
        .bind(i64::try_from(index).unwrap_or_default())
        .bind(i64::try_from(index).unwrap_or_default())
        .execute(pool)
        .await?;
        tree_ids.push(i64::try_from(result.last_insert_id()).context("任务 ID 超出 i64")?);
    }
    for chunk_start in (TREE_NODES..TASKS).step_by(1_000) {
        let chunk_end = (chunk_start + 1_000).min(TASKS);
        let mut query = QueryBuilder::<MySql>::new(
            "INSERT INTO work_task \
             (owner_user, project_project, parent_task, title, status, priority, created_at, updated_at) ",
        );
        query.push_values(chunk_start..chunk_end, |mut row, index| {
            row.push_bind(user_id)
                .push_bind(project_ids[index % PROJECTS])
                .push_bind(Option::<i64>::None)
                .push_bind(format!("任务-{index:05}"))
                .push_bind("todo")
                .push_bind("normal")
                .push_bind(i64::try_from(index).unwrap_or_default())
                .push_bind(i64::try_from(index).unwrap_or_default());
        });
        query.build().execute(pool).await?;
    }
    let seeded_tasks: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM work_task WHERE owner_user = ?")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    ensure!(
        seeded_tasks == i64::try_from(TASKS)?,
        "任务规模数据写入不完整"
    );
    let seed_ms = seed_started.elapsed().as_millis();

    let mut page_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let page = data(
            dispatch(
                &runtime,
                "work.task",
                "select",
                json!({
                    "page": 500,
                    "page_size": 100,
                    "order_by": [{ "field": "created_at", "direction": "Asc" }]
                }),
                &headers,
                &[],
            )
            .await?,
        )?;
        ensure!(
            page["items"]
                .as_array()
                .is_some_and(|items| items.len() == 100),
            "第 500 页必须稳定返回 100 条"
        );
        page_samples.push(started.elapsed().as_millis());
    }

    let mut relation_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let options = data(
            dispatch(
                &runtime,
                "work.project",
                "options",
                json!({
                    "search": "项目-09999",
                    "selected": [],
                    "filter": {},
                    "page": 1,
                    "limit": 20
                }),
                &headers,
                &[],
            )
            .await?,
        )?;
        ensure!(
            options["items"]
                .as_array()
                .is_some_and(|items| items.len() == 1),
            "一万项目中的关系搜索必须精确返回目标"
        );
        relation_samples.push(started.elapsed().as_millis());
    }

    let tree = data(
        dispatch(
            &runtime,
            "work.task",
            "select",
            json!({
                "page": 1,
                "page_size": 100,
                "order_by": [{ "field": "created_at", "direction": "Asc" }]
            }),
            &headers,
            &[],
        )
        .await?,
    )?;
    let tree_items = tree["items"].as_array().context("任务树响应缺少 items")?;
    ensure!(
        tree_items.len() == TREE_NODES,
        "任务树必须受 100 节点上限保护"
    );
    for (index, item) in tree_items.iter().enumerate().skip(1) {
        ensure!(
            item["parent_task"] == tree_items[index - 1]["id"],
            "100 层任务链必须保持父子关系"
        );
    }
    ensure!(
        dispatch(
            &runtime,
            "work.task",
            "select",
            json!({ "page": 1, "page_size": 101 }),
            &headers,
            &[],
        )
        .await
        .is_err(),
        "任务查询必须拒绝超过 100 的页面"
    );
    ensure!(
        dispatch(
            &runtime,
            "work.task",
            "put",
            json!({ "id": tree_ids[0], "data": { "parent_task": tree_ids[99] } }),
            &headers,
            &[],
        )
        .await
        .is_err(),
        "深层任务树必须拒绝形成关系环"
    );

    let race_a = data(
        dispatch(
            &runtime,
            "work.task",
            "add",
            json!({
                "project_project": tree_project,
                "title": "并发环检测-A",
                "status": "todo",
                "priority": "normal"
            }),
            &headers,
            &[],
        )
        .await?,
    )?["id"]
        .as_i64()
        .context("并发任务 A 缺少 id")?;
    let race_b = data(
        dispatch(
            &runtime,
            "work.task",
            "add",
            json!({
                "project_project": tree_project,
                "title": "并发环检测-B",
                "status": "todo",
                "priority": "normal"
            }),
            &headers,
            &[],
        )
        .await?,
    )?["id"]
        .as_i64()
        .context("并发任务 B 缺少 id")?;
    let (race_left, race_right) = tokio::join!(
        dispatch_token_body_action(
            &runtime,
            "work.task",
            "put",
            &access_token,
            json!({ "id": race_a, "data": { "parent_task": race_b } }),
        ),
        dispatch_token_body_action(
            &runtime,
            "work.task",
            "put",
            &access_token,
            json!({ "id": race_b, "data": { "parent_task": race_a } }),
        ),
    );
    let race_successes = [race_left, race_right]
        .into_iter()
        .filter(|result| result.as_ref().is_ok_and(|response| response.code == 0))
        .count();
    ensure!(
        race_successes == 1,
        "两个相反的并发父关系必须恰好一个成功，实际成功 {race_successes}"
    );
    let parent_a: Option<i64> =
        sqlx::query_scalar("SELECT parent_task FROM work_task WHERE id = ? AND owner_user = ?")
            .bind(race_a)
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    let parent_b: Option<i64> =
        sqlx::query_scalar("SELECT parent_task FROM work_task WHERE id = ? AND owner_user = ?")
            .bind(race_b)
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    ensure!(
        (parent_a == Some(race_b) && parent_b.is_none())
            || (parent_b == Some(race_a) && parent_a.is_none()),
        "并发父关系最终态必须无环且只保留一条边"
    );

    let other_username = format!("work_scale_other_{suffix}");
    let other = data(
        dispatch(
            &runtime,
            "account.user",
            "register",
            json!({ "username": other_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let other_user_id = other["id"].as_i64().context("对抗用户缺少 id")?;
    let other_login = data(
        dispatch(
            &runtime,
            "account.user",
            "login",
            json!({ "username": other_username, "password": password }),
            &[],
            &[],
        )
        .await?,
    )?;
    let other_authorization = format!(
        "Bearer {}",
        other_login["access_token"]
            .as_str()
            .context("对抗用户缺少 access_token")?
    );
    let other_headers = [("authorization", other_authorization.as_str())];
    let other_project = data(
        dispatch(
            &runtime,
            "work.project",
            "add",
            json!({ "name": "其他用户项目", "status": "active" }),
            &other_headers,
            &[],
        )
        .await?,
    )?;
    let other_task = data(
        dispatch(
            &runtime,
            "work.task",
            "add",
            json!({
                "project_project": other_project["id"],
                "title": "其他用户任务",
                "status": "todo",
                "priority": "normal"
            }),
            &other_headers,
            &[],
        )
        .await?,
    )?;
    let forged_tenant = other_user_id.to_string();
    ensure!(
        dispatch(
            &runtime,
            "work.task",
            "select",
            json!({ "page": 1, "page_size": 20 }),
            &[
                ("authorization", authorization.as_str()),
                ("x-tenant-id", forged_tenant.as_str())
            ],
            &[],
        )
        .await
        .is_err(),
        "个人工作区必须拒绝伪造其他用户 tenant"
    );

    let bulk_ids = tree_ids
        .iter()
        .take(100)
        .map(|id| json!({ "id": id }))
        .collect::<Vec<_>>();
    let bulk_started = Instant::now();
    let bulk = data(
        dispatch(
            &runtime,
            "work.task",
            "complete",
            json!({ "selected": bulk_ids }),
            &headers,
            &[],
        )
        .await?,
    )?;
    let bulk_ms = bulk_started.elapsed().as_millis();
    ensure!(
        bulk["requested"] == 100 && bulk["affected"] == 100,
        "100 条批量完成必须全量提交"
    );
    sqlx::query("UPDATE work_task SET status = 'todo' WHERE id = ? AND owner_user = ?")
        .bind(tree_ids[0])
        .bind(user_id)
        .execute(pool)
        .await?;
    ensure!(
        dispatch(
            &runtime,
            "work.task",
            "complete",
            json!({
                "selected": [
                    { "id": tree_ids[0] },
                    { "id": other_task["id"] }
                ]
            }),
            &headers,
            &[],
        )
        .await
        .is_err(),
        "混入其他工作区任务的批量更新必须整体失败"
    );
    let own_status: String =
        sqlx::query_scalar("SELECT status FROM work_task WHERE id = ? AND owner_user = ?")
            .bind(tree_ids[0])
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    ensure!(own_status == "todo", "失败批次不得部分提交当前用户任务");

    let concurrent_started = Instant::now();
    let mut requests = tokio::task::JoinSet::new();
    for page_number in 1..=10 {
        let runtime = Arc::clone(&runtime);
        let token = access_token.clone();
        requests.spawn(async move {
            dispatch_token_body_action(
                &runtime,
                "work.task",
                "select",
                &token,
                json!({ "page": page_number, "page_size": 100 }),
            )
            .await
        });
    }
    let mut completed = 0;
    while let Some(result) = requests.join_next().await {
        let response = result.context("并发查询任务 panic")??;
        ensure!(response.code == 0, "并发任务查询返回业务错误");
        completed += 1;
    }
    let concurrent_ms = concurrent_started.elapsed().as_millis();
    ensure!(completed == 10, "十路并发查询必须全部完成");

    let page_p95_ms = p95_millis(&mut page_samples);
    let relation_p95_ms = p95_millis(&mut relation_samples);
    ensure!(
        page_p95_ms <= 1_000,
        "深分页 p95 超过 1000ms: {page_p95_ms}"
    );
    ensure!(
        relation_p95_ms <= 1_000,
        "关系搜索 p95 超过 1000ms: {relation_p95_ms}"
    );
    ensure!(bulk_ms <= 2_000, "100 条批量完成超过 2000ms: {bulk_ms}");
    ensure!(
        concurrent_ms <= 5_000,
        "十路并发查询超过 5000ms: {concurrent_ms}"
    );
    println!(
        "work_scale_metrics projects={PROJECTS} tasks={TASKS} tree_nodes={TREE_NODES} \
         seed_ms={seed_ms} page_500_p95_ms={page_p95_ms} \
         relation_10000_p95_ms={relation_p95_ms} bulk_100_ms={bulk_ms} \
         concurrent_10_ms={concurrent_ms}"
    );

    tools.close().await;
    Ok(())
}
