use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing_subscriber::fmt::MakeWriter;
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

const WRONG_SECRET: &str = "wrong-bootstrap-secret-with-sufficient-length";

#[derive(Clone, Default)]
struct SharedLogWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedLogWriter {
    fn contents(&self) -> anyhow::Result<String> {
        let bytes = self
            .bytes
            .lock()
            .map_err(|_| anyhow::anyhow!("测试日志缓冲区锁已损坏"))?;
        String::from_utf8(bytes.clone()).context("测试日志不是合法 UTF-8")
    }
}

impl Write for SharedLogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| io::Error::other("测试日志缓冲区锁已损坏"))?;
        bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for SharedLogWriter {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

fn action_handle(app: &BuiltApp, module: &str, action: &str) -> Result<ActionHandle, BaseError> {
    let module = ModuleName::new(module)
        .map_err(|error| BaseError::ConfigError(format!("ModuleName 无效: {error}")))?;
    let action = ActionName::new(action)
        .map_err(|error| BaseError::ConfigError(format!("ActionName 无效: {error}")))?;
    let reference = ActionRef::new(module, action);
    app.registry().resolve(&reference).ok_or_else(|| {
        BaseError::ConfigError(format!("bootstrap 集成测试 Action 未注册: {reference}"))
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
    let peer: SocketAddr = "127.0.0.1:42000"
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

async fn build_harness(
    mysql_url: &str,
    redis_url: &str,
    verifier: BootstrapSecretVerifier,
) -> anyhow::Result<(Application, Arc<Tools>, sqlx::MySqlPool)> {
    let database_config = DatabaseConfig::default()
        .with_max_connections(8)
        .with_min_connections(0)
        .with_connect_timeout(10);
    let mysql = Database::connect_with_config(mysql_url, database_config.clone())
        .await
        .context("连接 bootstrap 测试 MySQL 失败")?;
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
    .context("连接 bootstrap 测试 Redis 失败")?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(redis)
            .token(TokenManager::new_symmetric(
                "bootstrap-integration-token-secret",
                Algorithm::HS256,
                "yang-system-bootstrap-integration".to_string(),
                "yang-system-bootstrap-api".to_string(),
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
    sqlx::raw_sql(include_str!(
        "../migrations/20260726_0006_create_authorization_outbox.sql"
    ))
    .execute(&pool)
    .await?;
    Ok((application, tools, pool))
}

async fn register_and_login(
    app: &BuiltApp,
    username: &str,
    password: &str,
) -> anyhow::Result<(i64, String)> {
    let registered = data(
        dispatch(
            app,
            "account.user",
            "register",
            json!({ "username": username, "password": password }),
            &[],
        )
        .await?,
    )?;
    let user_id = registered["id"].as_i64().context("注册响应缺少用户 id")?;
    let login = data(
        dispatch(
            app,
            "account.user",
            "login",
            json!({ "username": username, "password": password }),
            &[],
        )
        .await?,
    )?;
    let access_token = login["access_token"]
        .as_str()
        .context("登录响应缺少 access_token")?
        .to_string();
    Ok((user_id, access_token))
}

fn merge_outcome(outcome: anyhow::Result<()>, cleanup: anyhow::Result<()>) -> anyhow::Result<()> {
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("bootstrap 测试失败后清理也失败: {cleanup_error:#}")))
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn bootstrap_requires_operator_secret_and_is_single_use_under_concurrency(
) -> anyhow::Result<()> {
    let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let redis_url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        redis_url.trim_end_matches('/').ends_with("/15"),
        "集成测试 Redis URL 必须使用独立 DB 15"
    );

    let generated = generate_bootstrap_secret()?;
    let secret = generated.secret().to_owned();
    let digest = generated.digest().as_str().to_owned();
    let verifier = BootstrapSecretVerifier::new(generated.digest().clone(), 2)?;
    let (application, tools, pool) = build_harness(&mysql_url, &redis_url, verifier).await?;
    let log_writer = SharedLogWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(log_writer.clone())
        .finish();
    let default_guard = tracing::subscriber::set_default(subscriber);

    let outcome = async {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let password = "correct-horse-battery-staple";
        let (first_id, first_token) = register_and_login(
            &application.runtime,
            &format!("bootstrap_a_{suffix}"),
            password,
        )
        .await?;
        let (second_id, second_token) = register_and_login(
            &application.runtime,
            &format!("bootstrap_b_{suffix}"),
            password,
        )
        .await?;
        let first_authorization = format!("Bearer {first_token}");
        let second_authorization = format!("Bearer {second_token}");

        let wrong = dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({ "secret": WRONG_SECRET, "name": "Wrong Credential" }),
            &[("authorization", &first_authorization)],
        )
        .await;
        ensure!(
            matches!(wrong, Err(BaseError::Unauthorized(_))),
            "错误 bootstrap secret 必须返回 Unauthorized: {wrong:?}"
        );
        let after_wrong: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `admin_user`")
            .fetch_one(&pool)
            .await?;
        ensure!(after_wrong == 0, "错误 secret 不得写入平台账号");

        let first_headers = [("authorization", first_authorization.as_str())];
        let second_headers = [("authorization", second_authorization.as_str())];
        let first = dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({ "secret": secret, "name": "First Concurrent Operator" }),
            &first_headers,
        );
        let second = dispatch(
            &application.runtime,
            "admin.user",
            "bootstrap",
            json!({ "secret": secret, "name": "Second Concurrent Operator" }),
            &second_headers,
        );
        let (first_result, second_result) = tokio::join!(first, second);
        let successes = usize::from(first_result.is_ok()) + usize::from(second_result.is_ok());
        ensure!(
            successes == 1,
            "并发 bootstrap 必须恰好一个成功: first={first_result:?}, second={second_result:?}"
        );

        let rows: Vec<(i64, String, bool, String)> =
            sqlx::query_as("SELECT user_user, status, admin, bootstrap_key FROM `admin_user`")
                .fetch_all(&pool)
                .await?;
        ensure!(rows.len() == 1, "并发后必须只有一个平台账号: {rows:?}");
        ensure!(
            [first_id, second_id].contains(&rows[0].0)
                && rows[0].1 == "active"
                && rows[0].2
                && rows[0].3 == "initial-admin",
            "成功记录必须保持一次性初始化数据库不变量: {rows:?}"
        );

        for authorization in [&first_authorization, &second_authorization] {
            let replay = dispatch(
                &application.runtime,
                "admin.user",
                "bootstrap",
                json!({ "secret": secret, "name": "Replay Operator" }),
                &[("authorization", authorization)],
            )
            .await;
            ensure!(
                replay.is_err(),
                "成功后的 bootstrap secret 重放必须永久失败"
            );
        }
        let after_replay: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `admin_user`")
            .fetch_one(&pool)
            .await?;
        ensure!(after_replay == 1, "重放不得新增平台账号");
        Ok(())
    }
    .await;

    drop(default_guard);
    let log_output = log_writer.contents()?;
    let log_verification = (|| {
        ensure!(
            log_output.contains("credential_rejected")
                && log_output.contains("succeeded")
                && log_output.contains("failed"),
            "日志必须记录错误凭证、成功与失败结果: {log_output}"
        );
        for sensitive in [&secret, &digest, WRONG_SECRET] {
            ensure!(
                !log_output.contains(sensitive),
                "bootstrap 日志不得泄露 secret 或摘要"
            );
        }
        Ok(())
    })();
    let outcome = outcome.and(log_verification);
    let cleanup = reset_test_database(&pool).await;
    tools.close().await;
    merge_outcome(outcome, cleanup)
}
