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
use yang_system::bootstrap_secret::{generate_bootstrap_secret, BootstrapSecretVerifier};
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
    let generated_bootstrap = generate_bootstrap_secret()?;
    let bootstrap_secret = generated_bootstrap.secret().to_owned();
    let bootstrap_verifier = BootstrapSecretVerifier::new(generated_bootstrap.digest().clone(), 2)?;
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
            .config(bootstrap_verifier)
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
    sqlx::raw_sql(include_str!(
        "../migrations/20260726_0006_create_authorization_outbox.sql"
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
    let login_authz_version = token_authz_version(&tools, access_token)?;
    ensure!(
        login_authz_version == token_authz_version(&tools, refresh_token)?,
        "同次登录签发的 Access/Refresh Token 必须携带同一授权版本"
    );
    let bootstrap = data(
        dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({
                "secret": bootstrap_secret,
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
    ensure!(
        dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({ "secret": bootstrap_secret, "name": "Second Administrator" }),
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
    ensure!(
        token_authz_version(&tools, admin_access_token)?
            == token_authz_version(&tools, admin_refresh_token)?,
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
            json!({ "username": member_username, "password": password }),
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
    for admin in [true, false] {
        data(
            dispatch(
                &application.runtime,
                "admin.user",
                "set_admin",
                json!({ "id": platform_member_id, "admin": admin }),
                &[("authorization", &admin_authorization)],
                &[],
            )
            .await?,
        )?;
    }
    ensure!(
        database_authz_version(tools.mysql()?.pool(), member_id).await?
            == member_version_after_idempotent_admin + 2,
        "超级管理员授予与撤销必须各递增一次授权版本"
    );

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
    let invalid_outbox_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authorization_outbox \
         WHERE state <> 'pending' OR attempts <> 0 OR available_at <= 0 OR created_at <= 0 \
            OR lease_until IS NOT NULL OR worker_id IS NOT NULL \
            OR published_at IS NOT NULL OR last_error IS NOT NULL",
    )
    .fetch_one(tools.mysql()?.pool())
    .await?;
    ensure!(
        invalid_outbox_rows == 0,
        "事务 writer 只能写入可立即派发的纯净 pending 事件"
    );

    tools.close().await;
    Ok(())
}
