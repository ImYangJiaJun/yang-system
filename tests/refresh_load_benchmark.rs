mod common;

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::task::JoinSet;
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::SecuritySettings;
use yang_system::schema::sync_with_database;

use common::{take_registration_code, RegistrationEmailToolsExt};

const CONCURRENCY: usize = 10;
const REFRESHES_PER_SESSION: usize = 100;
const BENCHMARK_PASSWORD: &str = "correct-horse-battery-staple";

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

fn security_settings() -> Arc<SecuritySettings> {
    Arc::new(SecuritySettings {
        argon2_max_concurrency: 4,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 10_000,
        auth_rate_limit_username_attempts: 1_000,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: true,
        trusted_proxy_cidrs: Vec::new(),
    })
}

fn token_manager() -> TokenManager {
    TokenManager::new_symmetric_keyring(
        "refresh-benchmark-active".to_string(),
        "refresh-benchmark-secret-at-least-32-bytes",
        Vec::new(),
        Algorithm::HS256,
        "yang-system-refresh-benchmark".to_string(),
        "yang-system-refresh-benchmark-api".to_string(),
        3600,
        2_592_000,
    )
    .unwrap_or_else(|error| panic!("Refresh 基准 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "refresh-benchmark-step-up-secret-32-bytes",
            "yang-system-refresh-benchmark-step-up",
            "yang-system-refresh-benchmark-sensitive",
        )
        .unwrap_or_else(|error| panic!("Refresh 基准 Step-up manager 应构建成功: {error}")),
    )
}

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接 Refresh 基准 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取 Refresh 基准数据库名失败")?;
    let name = name.context("Refresh 基准连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行 Refresh 基准"
    );
    Ok(database)
}

async fn connect_test_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "Refresh 基准 Redis URL 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, redis_config())
        .await
        .context("连接 Refresh 基准 Redis 失败")
}

async fn reset_database(database: &Database) -> anyhow::Result<()> {
    for table in [
        "password_reset_token",
        "audit_event",
        "authorization_outbox",
        "users",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(database.pool())
            .await
            .with_context(|| format!("清理 Refresh 基准表失败: {table}"))?;
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
) -> anyhow::Result<ApiResponse> {
    let mut request = Request::new(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], peer_port))),
    );
    let response = app
        .dispatch_context(action_handle(app, module, action)?, context)
        .await
        .with_context(|| format!("{module}.{action} 调用失败"))?;
    ensure!(
        response.code == 0,
        "{module}.{action} 返回业务错误 {}: {}",
        response.code,
        response.message
    );
    Ok(response)
}

fn response_data(response: &ApiResponse) -> anyhow::Result<&Value> {
    response.data.as_ref().context("Action 成功响应缺少 data")
}

fn refresh_token(response: &ApiResponse) -> anyhow::Result<String> {
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
        .context("Refresh 响应缺少轮换后的 yang_refresh Cookie")
}

fn percentile(samples: &[u128], percentile: usize) -> u128 {
    let index = (samples.len().saturating_sub(1) * percentile) / 100;
    samples[index]
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    database_cleanup: anyhow::Result<()>,
    redis_cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, database_cleanup, redis_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) => Err(error.context("Refresh 基准数据库清理失败")),
        (Ok(()), Ok(()), Err(error)) => Err(error.context("Refresh 基准 Redis 清理失败")),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL；测量真实 Refresh 路径 p50/p95/p99"]
async fn refresh_rotation_load_has_zero_errors_and_reports_percentiles() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    let redis = connect_test_redis().await?;
    reset_database(&control).await?;
    reset_redis(&redis).await?;

    let outcome = async {
        sync_with_database(
            connect_test_database().await?,
            database_config(),
            security_settings(),
        )
        .await?;
        let deployment = format!(
            "refresh-benchmark-{}",
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
        let app = build_app(tools, security_settings())?;

        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let mut sessions = Vec::with_capacity(CONCURRENCY);
        for index in 0..CONCURRENCY {
            let username = format!("refresh_benchmark_{suffix}_{index}");
            let email = format!("{username}@example.test");
            dispatch(
                &app.runtime,
                "account.user",
                "request_registration_email",
                json!({ "email": email }),
                &[],
                42_000 + u16::try_from(index)?,
            )
            .await?;
            let email_code = take_registration_code(&email)?;
            dispatch(
                &app.runtime,
                "account.user",
                "register",
                json!({
                    "username": username,
                    "password": BENCHMARK_PASSWORD,
                    "email": email,
                    "email_code": email_code,
                }),
                &[],
                42_000 + u16::try_from(index)?,
            )
            .await?;
            let login = dispatch(
                &app.runtime,
                "account.user",
                "login",
                json!({ "username": username, "password": BENCHMARK_PASSWORD }),
                &[],
                42_000 + u16::try_from(index)?,
            )
            .await?;
            ensure!(
                response_data(&login)?["access_token"].as_str().is_some(),
                "登录响应缺少 Access Token"
            );
            sessions.push(refresh_token(&login)?);
        }

        let started = Instant::now();
        let mut tasks = JoinSet::new();
        for (index, initial_refresh) in sessions.into_iter().enumerate() {
            let runtime = app.runtime.clone();
            tasks.spawn(async move {
                let mut token = initial_refresh;
                let mut samples = Vec::with_capacity(REFRESHES_PER_SESSION);
                for _ in 0..REFRESHES_PER_SESSION {
                    let cookie = format!("yang_refresh={token}");
                    let request_started = Instant::now();
                    let response = dispatch(
                        &runtime,
                        "account.user",
                        "refresh",
                        json!({}),
                        &[("cookie", cookie.as_str())],
                        42_000 + u16::try_from(index)?,
                    )
                    .await?;
                    samples.push(request_started.elapsed().as_micros());
                    token = refresh_token(&response)?;
                }
                anyhow::Ok(samples)
            });
        }

        let mut samples = Vec::with_capacity(CONCURRENCY * REFRESHES_PER_SESSION);
        while let Some(result) = tasks.join_next().await {
            samples.extend(result.context("Refresh 基准任务 panic")??);
        }
        let elapsed = started.elapsed();
        ensure!(
            samples.len() == CONCURRENCY * REFRESHES_PER_SESSION,
            "Refresh 成功样本数不完整: {}",
            samples.len()
        );
        samples.sort_unstable();
        let p50 = percentile(&samples, 50);
        let p95 = percentile(&samples, 95);
        let p99 = percentile(&samples, 99);
        if let Some(budget_ms) = std::env::var("YANG_SYSTEM_REFRESH_P99_BUDGET_MS")
            .ok()
            .map(|value| value.parse::<u128>())
            .transpose()
            .context("YANG_SYSTEM_REFRESH_P99_BUDGET_MS 必须是正整数")?
        {
            ensure!(budget_ms > 0, "Refresh p99 预算必须大于 0ms");
            ensure!(
                p99 <= budget_ms * 1_000,
                "Refresh p99={}us 超过 staging 预算 {}ms",
                p99,
                budget_ms
            );
        }
        let throughput = samples.len() as f64 / elapsed.as_secs_f64();
        eprintln!(
            "refresh rotation benchmark: sessions={CONCURRENCY}, requests={}, errors=0, elapsed_ms={}, throughput_rps={throughput:.2}, p50_us={p50}, p95_us={p95}, p99_us={p99}",
            samples.len(),
            elapsed.as_millis()
        );
        Ok(())
    }
    .await;

    let database_cleanup = reset_database(&control).await;
    let redis_cleanup = reset_redis(&redis).await;
    finish_with_cleanup(outcome, database_cleanup, redis_cleanup)
}
