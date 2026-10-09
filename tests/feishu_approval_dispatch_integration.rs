//! 审批派发控制台（Task C 的 7 个 Action）与派发落记录的端到端集成测试。
//!
//! # 覆盖什么
//!
//! 真实 MySQL + Redis 下，把**整条链路**走一遍：
//!
//! - dispatch 各出口落 `feishu_approval_request_log`：校验失败 / 未配置 / 批量受理 /
//!   provision 失败，断言 outcome 正确性与行存在性；
//! - 配置 CRUD：list_configs（映射分组、不投影大字段）→ update_config（三件套 +
//!   审计）→ delete_config（清 pending 任务、保留完结任务 + 审计）；
//! - create_config / list_widgets 的凭证缺失出口（50301）；
//! - list_requests / list_tasks 的分页与过滤。
//!
//! # 覆盖不到的（如实说明）
//!
//! `dispatch_single` 的单条**成功/等待**出口与 `create_config` / `list_widgets` 的
//! **成功**路径需要出站直打飞书（`https://open.feishu.cn` 硬编码在
//! `tenant_token.rs`，传输不可注入）——它们由 crate 内 `dispatch.rs` 的
//! ReplayTransport 端到端单测覆盖（`end_to_end_provision_then_dispatch_then_backfill`）。
//! 本文件覆盖同一条链路上**能落到真实库**的部分：provision 失败（dummy 凭证
//! 必然在取 token 一步失败，落 failed 行）与全部纯库行为。
//!
//! # 运行方式
//!
//! ```text
//! YANG_SYSTEM_TEST_DATABASE_URL=mysql://root:yang-local@127.0.0.1:3306/yang_system_test \
//! YANG_SYSTEM_TEST_REDIS_URL=redis://127.0.0.1:6379/15 \
//! cargo test --test feishu_approval_dispatch_integration -- --ignored --test-threads=1
//! ```
//!
//! 本文件**读取** `YANG_SYSTEM_TEST_` 开头的环境变量，`scripts/run_ci.py` 的反向发现
//! 因此会要求把它登记进 `INTEGRATION`——漏登记 `--self-test` 会失败。

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::{FeishuSettings, SecuritySettings};
use yang_system::feishu_approval_worker::ApprovalDispatchHandle;
use yang_system::schema::sync_with_database;

/// 多维表格写入接口的管理 Token（dispatch 的鉴权）。
const MANAGEMENT_TOKEN: &str = "integration-management-token";

// ---------------------------------------------------------------- 连接与清理

async fn connect_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, DatabaseConfig::default())
        .await
        .context("连接测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取测试数据库名失败")?;
    let name = name.context("测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行审批派发集成测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "审批派发集成测试 Redis 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, RedisConfig::default())
        .await
        .map_err(Into::into)
}

/// 清空测试库：先删业务表再删账号表（外键 RESTRICT 的顺序问题与
/// `permission_groups_integration` 同套）；审批四表无外键、顺序无关。
async fn reset_database(database: &Database) -> anyhow::Result<()> {
    for table in [
        "user_group",
        "authz_grant",
        "permission_group_item",
        "permission_group",
        "system_owner",
        "user_avatar",
        "password_reset_token",
        "user_session",
        "login_event",
        "audit_event",
        "authorization_outbox",
        "feishu_approval_request_log",
        "feishu_approval_task",
        "feishu_approval_field_map",
        "feishu_approval_config",
        "users",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
            .execute(database.pool())
            .await
            .with_context(|| format!("清理测试表失败: {table}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- 应用装配

fn database_config() -> DatabaseConfig {
    DatabaseConfig::default()
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
        totp: None,
    })
}

/// 飞书设置；`app_id` / `app_secret` 为 `None` 时 `can_pull()` 为假（dispatch 路由
/// 不注册、出站路径整体不参与——create_config 的 50301 用例就用它）。
fn feishu_settings(app_id: Option<&str>, app_secret: Option<&str>) -> Arc<FeishuSettings> {
    Arc::new(FeishuSettings {
        enabled: true,
        management_api_token: MANAGEMENT_TOKEN.to_string(),
        encryption_key: None,
        app_id: app_id.map(str::to_string),
        app_secret: app_secret.map(str::to_string),
        pull_interval_seconds: 900,
        alert_recipients: Vec::new(),
        alert_failure_threshold: 3,
        log_inbound_requests: false,
        approval_create_rate_per_minute: 90,
        approval_scan_interval_seconds: 30,
        approval_base_timezone: "Asia/Shanghai".to_string(),
    })
}

fn token_manager() -> TokenManager {
    TokenManager::new_symmetric(
        "feishu-approval-integration-token-secret-32",
        Algorithm::HS256,
        "feishu-approval-integration".to_string(),
        "feishu-approval-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
}

/// 装配一个启用飞书集成的完整应用。
///
/// `dispatch_handle` 为 `Some` 时把审批派发句柄注入 Tools（批量受理出口要它）；
/// 生产装配由 `bootstrap.rs` 做同一件事。授权版本缓存用**每次运行唯一**的
/// namespace：复用固定 namespace 会让上一次运行留下的版本号参与本轮判定。
async fn build_feishu_app(
    database: &Database,
    redis: &RedisClient,
    settings: Arc<FeishuSettings>,
    dispatch_handle: Option<ApprovalDispatchHandle>,
) -> anyhow::Result<Arc<BuiltApp>> {
    let namespace = format!(
        "feishu-approval-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let mut builder = ToolsBuilder::new()
        .mysql(Database::from_pool(
            database.pool().clone(),
            database_config(),
        )?)
        .cache(redis.clone())
        .extension(AuthorizationVersionCache::new(redis.clone(), namespace)?)
        .token(token_manager())
        .config(settings);
    if let Some(handle) = dispatch_handle {
        builder = builder.extension(handle);
    }
    let tools = Arc::new(builder.build()?);
    let application = build_app(tools, security_settings())?;
    Ok(Arc::new(application.runtime))
}

/// 种一个「控制台操作者」用户并直接签发带权限的 Access Token。
///
/// 与 `feishu_xlsx_import_integration::seed_operator` 同套路：本文件不测登录链路，
/// 直接签一张含 `feishu.approval.read` / `write` 的令牌。
async fn seed_operator(database: &Database) -> anyhow::Result<String> {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let username = format!("approval_operator_{unique}");
    let result = sqlx::query(
        "INSERT INTO `users` \
         (`username`, `password_hash`, `status`, `authz_version`, `created_at`, `updated_at`) \
         VALUES (?, 'not-a-real-password-hash', 'active', 1, NOW(), NOW())",
    )
    .bind(&username)
    .execute(database.pool())
    .await
    .context("种入审批控制台操作者失败")?;
    let user_id = result.last_insert_id() as i64;

    token_manager()
        .generate_access_token(
            &user_id.to_string(),
            json!({
                "version": 1,
                "username": username,
                "authz_version": 1,
                "roles": ["user"],
                "permissions": ["feishu.approval.read", "feishu.approval.write"],
            }),
        )
        .map_err(|error| anyhow::anyhow!("签发控制台操作者令牌失败: {error}"))
}

/// 清理 + 同步 Schema + 重连 + 种操作者 + 装配应用，返回 `(database, app, token)`。
///
/// `sync_with_database` 会关掉连接池，所以同步之后**必须重连**。
async fn prepare_app(
    app_id: Option<&str>,
    app_secret: Option<&str>,
) -> anyhow::Result<(Database, Arc<BuiltApp>, String)> {
    let database = connect_database().await?;
    reset_database(&database).await?;
    sync_with_database(database, database_config(), security_settings())
        .await
        .context("同步 Schema 失败")?;
    let database = connect_database().await?;
    let redis = connect_redis().await?;
    let token = seed_operator(&database).await?;
    let app =
        build_feishu_app(&database, &redis, feishu_settings(app_id, app_secret), None).await?;
    Ok((database, app, token))
}

// ---------------------------------------------------------------- 派发辅助

/// 解析一个 Action 的派发句柄。
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

/// 经 Registry 派发一次请求（等价于 HTTP 层走到 Action 之前的那一段）。
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
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], 41_001))),
    );
    let handle = action_handle(app, module, action)
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    app.dispatch_context(handle, context).await
}

/// 以控制台操作者身份派发一个控制台 Action。
async fn dispatch_as_operator(
    app: &BuiltApp,
    action: &str,
    token: &str,
    body: Value,
) -> anyhow::Result<ApiResponse> {
    let authorization = format!("Bearer {token}");
    dispatch(
        app,
        "feishu.approval",
        action,
        body,
        &[("authorization", authorization.as_str())],
    )
    .await
    .with_context(|| format!("派发 {action} 不应返回 Err"))
}

fn response_data(response: &ApiResponse) -> anyhow::Result<Value> {
    response.data.clone().context("响应里没有 data")
}

/// 读 `feishu_approval_request_log` 里指定坐标的最近一条，返回 `(outcome, config_id, message)`。
async fn latest_request_log(
    database: &Database,
    base_token: &str,
) -> anyhow::Result<(String, Option<i64>, String)> {
    let row = sqlx::query(
        "SELECT `outcome`, `config_id`, `message` \
         FROM `feishu_approval_request_log` WHERE `base_token` = ? ORDER BY `id` DESC LIMIT 1",
    )
    .bind(base_token)
    .fetch_optional(database.pool())
    .await
    .context("读取派发请求记录失败")?
    .context("应存在请求记录行")?;
    use sqlx::Row;
    Ok((row.get("outcome"), row.get("config_id"), row.get("message")))
}

// ---------------------------------------------------------------- 数据播种

/// 播种一条启用中的配置行，返回 `config_id`。
async fn seed_config(
    database: &Database,
    base_token: &str,
    table_id: &str,
    enabled: bool,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO `feishu_approval_config` \
         (`title`, `base_token`, `table_id`, `approval_code`, `applicant_field`, `backfill_field`, \
          `base_timezone`, `enabled`, `created_at`, `updated_at`) \
         VALUES ('夹具配置', ?, ?, 'CODE-FIXTURE', 'fldP', 'fldB', 'Asia/Shanghai', ?, NOW(), NOW())",
    )
    .bind(base_token)
    .bind(table_id)
    .bind(enabled)
    .execute(database.pool())
    .await
    .context("种入审批配置失败")?;
    Ok(result.last_insert_id() as i64)
}

/// 播种一条字段映射行。
async fn seed_field_map(
    database: &Database,
    config_id: i64,
    widget_id: &str,
    bitable_field: &str,
    required: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO `feishu_approval_field_map` \
         (`config_id`, `widget_id`, `widget_type`, `required`, `bitable_field`, \
          `bitable_field_name`, `converter`, `created_at`, `updated_at`) \
         VALUES (?, ?, 'input', ?, ?, ?, 'direct', NOW(), NOW())",
    )
    .bind(config_id)
    .bind(widget_id)
    .bind(required)
    .bind(bitable_field)
    .bind(bitable_field)
    .execute(database.pool())
    .await
    .context("种入字段映射失败")?;
    Ok(())
}

/// 播种一条派发任务行。
async fn seed_task(
    database: &Database,
    config_id: i64,
    record_id: &str,
    state: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO `feishu_approval_task` \
         (`config_id`, `record_id`, `uuid`, `state`, `attempts`, `created_at`, `updated_at`) \
         VALUES (?, ?, ?, ?, 0, NOW(), NOW())",
    )
    .bind(config_id)
    .bind(record_id)
    .bind(format!("uuid-{record_id}"))
    .bind(state)
    .execute(database.pool())
    .await
    .with_context(|| format!("种入任务 {record_id} 失败"))?;
    Ok(())
}

/// 播种一条派发请求记录行。
async fn seed_request_log(
    database: &Database,
    base_token: &str,
    outcome: &str,
    message: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO `feishu_approval_request_log` \
         (`requested_by`, `base_token`, `table_id`, `request_body`, `outcome`, `message`, `created_at`) \
         VALUES ('集成测试', ?, 'tblSeed', '{}', ?, ?, NOW())",
    )
    .bind(base_token)
    .bind(outcome)
    .bind(message)
    .execute(database.pool())
    .await
    .context("种入请求记录失败")?;
    Ok(())
}

// ---------------------------------------------------------------- 用例

/// dispatch 校验失败也要落一条 `failed` 记录——而且请求人归一照旧生效。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn dispatch_validation_failure_writes_a_failed_request_log() {
    let outcome = async {
        let (database, app, _token) =
            prepare_app(Some("cli_test_app"), Some("test-app-secret")).await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");

        let result = dispatch(
            &app,
            "feishu.approval",
            "dispatch_approval",
            json!({ "base_token": "   ", "table_id": "tblX", "record_id": "rec1" }),
            &[("authorization", bearer.as_str())],
        )
        .await;
        ensure!(
            result.is_err(),
            "校验失败必须冒泡为 Err（HTTP 400），实际: {result:?}"
        );

        let (outcome, config_id, message) = latest_request_log(&database, "").await?;
        ensure!(
            outcome == "failed",
            "校验失败必须落 failed 桶，实际 {outcome}"
        );
        ensure!(
            config_id.is_none(),
            "校验失败时 config_id 必须为空（配置都还没查）"
        );
        ensure!(
            message.contains("不能为空"),
            "失败记录的消息要可行动，实际 {message}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "校验失败落记录").await;
}

/// 配置不存在且未给三件套：40401 业务失败，同样落 failed 行（不出网）。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn dispatch_without_config_reports_40401_and_writes_a_failed_log() {
    let outcome = async {
        let (database, app, _token) =
            prepare_app(Some("cli_test_app"), Some("test-app-secret")).await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");

        let response = dispatch(
            &app,
            "feishu.approval",
            "dispatch_approval",
            json!({ "base_token": "appNoConfig", "table_id": "tblNoConfig", "record_id": "rec1" }),
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("未配置出口不应返回 Err——那是业务失败，走 200 + fail 信封")?;
        ensure!(response.code == 40401, "未配置必须给出 40401: {response:?}");

        let (outcome, config_id, message) = latest_request_log(&database, "appNoConfig").await?;
        ensure!(outcome == "failed", "实际 {outcome}");
        ensure!(config_id.is_none(), "配置没建成，关联必须为空");
        ensure!(
            message.contains("未配置审批派发"),
            "失败记录的消息要可行动，实际 {message}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "未配置出口落记录").await;
}

/// 批量受理：白名单通过后落 `accepted` 行，config_id 关联、请求人归一保留。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn dispatch_batch_acceptance_writes_an_accepted_request_log() {
    let outcome = async {
        let database = connect_database().await?;
        reset_database(&database).await?;
        sync_with_database(database, database_config(), security_settings())
            .await
            .context("同步 Schema 失败")?;
        let database = connect_database().await?;
        let redis = connect_redis().await?;
        let config_id = seed_config(&database, "appAccTest", "tblAccTest", true).await?;

        // 注入派发句柄：接收端保持存活（worker 没起也没关系，信号进了通道即可）。
        let (handle, _receiver) = ApprovalDispatchHandle::new();
        let app = build_feishu_app(
            &database,
            &redis,
            feishu_settings(Some("cli_test_app"), Some("test-app-secret")),
            Some(handle),
        )
        .await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");

        let response = dispatch(
            &app,
            "feishu.approval",
            "dispatch_approval",
            json!({ "base_token": "appAccTest", "table_id": "tblAccTest", "requested_by": "批量按钮" }),
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("批量受理不应返回 Err")?;
        ensure!(response.code == 0, "批量受理必须成功: {response:?}");
        let data = response_data(&response)?;
        ensure!(
            data["accepted"] == json!(true),
            "批量受理的 accepted 必须为 true: {data}"
        );

        let (outcome, row_config_id, message) = latest_request_log(&database, "appAccTest").await?;
        ensure!(outcome == "accepted", "批量受理必须落 accepted 桶，实际 {outcome}");
        ensure!(
            row_config_id == Some(config_id),
            "受理行的 config_id 必须关联到白名单命中的配置，实际 {row_config_id:?}"
        );
        ensure!(
            message.contains("已受理"),
            "受理行的消息要可行动，实际 {message}"
        );

        let requested_by: Option<String> =
            sqlx::query_scalar("SELECT `requested_by` FROM `feishu_approval_request_log` LIMIT 1")
                .fetch_one(database.pool())
                .await
                .context("读取请求人失败")?;
        ensure!(
            requested_by.as_deref() == Some("批量按钮"),
            "请求人必须原样落库，实际 {requested_by:?}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "批量受理落记录").await;
}

/// provision 失败（dummy 凭证必然在取 token 一步失败）：35600 业务失败 + failed 行。
///
/// 全表受理（无 record_id）时不尝试回填，链路更干净；出一网一次但结果确定。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn dispatch_provision_failure_writes_a_failed_request_log() {
    let outcome = async {
        let (database, app, _token) =
            prepare_app(Some("cli_dummy_app"), Some("dummy-secret")).await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");

        let response = dispatch(
            &app,
            "feishu.approval",
            "dispatch_approval",
            json!({
                "base_token": "appProvFail",
                "table_id": "tblProvFail",
                "approval_code": "CODE-PROV",
                "applicant_field": "申请人",
                "backfill_field": "审批编号",
            }),
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("provision 失败不应返回 Err——走 200 + 35600 信封")?;
        ensure!(
            response.code == 35600,
            "provision 失败必须给出 35600，实际: {response:?}"
        );

        let (outcome, config_id, message) = latest_request_log(&database, "appProvFail").await?;
        ensure!(
            outcome == "failed",
            "provision 失败必须落 failed 桶，实际 {outcome}"
        );
        ensure!(config_id.is_none(), "配置没建成，关联必须为空");
        ensure!(
            message.contains("取飞书数据失败") || message.contains("读本库配置失败"),
            "失败记录要带可归因原因，实际 {message}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "provision 失败落记录").await;
}

/// create_config 凭证缺失 → 50301（照 dispatch provision 的处置），不出网。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn create_config_requires_outbound_credentials() {
    let outcome = async {
        let (_database, app, token) = prepare_app(None, None).await?;

        let response = dispatch_as_operator(
            &app,
            "create_config",
            &token,
            json!({
                "base_token": "appX",
                "table_id": "tblX",
                "approval_code": "CODE-X",
                "applicant_field": "申请人",
                "backfill_field": "审批编号",
                "base_timezone": "Asia/Shanghai",
            }),
        )
        .await?;
        ensure!(
            response.code == 50301,
            "凭证缺失必须给出 50301，实际: {response:?}"
        );
        ensure!(
            response.data.is_none(),
            "失败信封不得携带 data: {response:?}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "create_config 凭证缺失").await;
}

/// 配置 CRUD 闭环：list_configs（映射分组、不投影大字段）→ update_config（三件套 +
/// 审计）→ delete_config（+ 审计），以及不可改键与缺失配置的明确失败。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn console_crud_loop_on_a_seeded_config() {
    let outcome = async {
        let (database, app, token) = prepare_app(None, None).await?;
        let config_id = seed_config(&database, "appCrud", "tblCrud", true).await?;
        seed_field_map(&database, config_id, "w1", "fldCompany", true).await?;
        seed_field_map(&database, config_id, "w2", "fldAmount", false).await?;

        // ---- list_configs：映射分组 + 不投影 form_snapshot ----
        let listed = dispatch_as_operator(
            &app,
            "list_configs",
            &token,
            json!({ "count_total": true }),
        )
        .await?;
        ensure!(listed.code == 0, "list_configs 必须成功: {listed:?}");
        let data = response_data(&listed)?;
        let items = data["items"]
            .as_array()
            .context("items 必须是数组")?;
        ensure!(items.len() == 1, "应只回一条配置，实际 {}", items.len());
        let item = &items[0];
        ensure!(item["id"] == json!(config_id), "id 必须对得上");
        ensure!(item["title"] == json!("夹具配置"));
        ensure!(item["maps"].as_array().map(Vec::len) == Some(2), "两条映射都要带出");
        ensure!(
            !item.to_string().contains("\"form_snapshot\""),
            "列表项不得投影 form_snapshot 大字段"
        );
        ensure!(
            data["total"] == json!(1),
            "count_total 要真的数出来: {data}"
        );

        // ---- update_config：三件套生效 + 审计 ----
        let updated = dispatch_as_operator(
            &app,
            "update_config",
            &token,
            json!({
                "config_id": config_id,
                "title": "改名后的配置",
                "enabled": false,
                "base_timezone": "UTC",
            }),
        )
        .await?;
        ensure!(updated.code == 0, "update_config 必须成功: {updated:?}");
        let row: (String, i64, String) = sqlx::query_as(
            "SELECT `title`, `enabled`, `base_timezone` FROM `feishu_approval_config` WHERE `id` = ?",
        )
        .bind(config_id)
        .fetch_one(database.pool())
        .await
        .context("读取更新后的配置失败")?;
        ensure!(row.0 == "改名后的配置" && row.1 == 0 && row.2 == "UTC", "三件套都要生效: {row:?}");
        let audit_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM `audit_event` WHERE `action` = 'feishu.approval.update_config' AND `result` = 'succeeded'")
                .fetch_one(database.pool())
                .await
                .context("统计更新审计失败")?;
        ensure!(audit_rows == 1, "update_config 必须落一条成功审计: {audit_rows}");

        // ---- 坐标与三件套不可改：deny_unknown_fields 显式拒绝 ----
        let operator = format!("Bearer {token}");
        let rejected = dispatch(
            &app,
            "feishu.approval",
            "update_config",
            json!({ "config_id": config_id, "base_token": "appX" }),
            &[("authorization", operator.as_str())],
        )
        .await;
        ensure!(
            rejected.is_err(),
            "带 base_token 的更新必须被拒（改坐标 = 删了重建）: {rejected:?}"
        );

        // ---- 缺失配置的明确失败 ----
        let missing = dispatch(
            &app,
            "feishu.approval",
            "update_config",
            json!({ "config_id": 9_999_999, "title": "不存在" }),
            &[("authorization", operator.as_str())],
        )
        .await;
        ensure!(
            matches!(&missing, Err(BaseError::RecordNotFound(_))),
            "缺失配置必须明确 404，实际: {missing:?}"
        );

        // ---- delete_config：删配置 + 映射 + 审计 ----
        let deleted = dispatch_as_operator(
            &app,
            "delete_config",
            &token,
            json!({ "config_id": config_id }),
        )
        .await?;
        ensure!(deleted.code == 0, "delete_config 必须成功: {deleted:?}");
        let data = response_data(&deleted)?;
        ensure!(
            data["deleted_field_maps"] == json!(2),
            "两条映射都要删掉: {data}"
        );
        let config_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `feishu_approval_config`")
            .fetch_one(database.pool())
            .await
            .context("统计配置行失败")?;
        ensure!(config_rows == 0, "配置行必须删干净");
        let audit_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM `audit_event` WHERE `action` = 'feishu.approval.delete_config' AND `result` = 'succeeded'")
                .fetch_one(database.pool())
                .await
                .context("统计删除审计失败")?;
        ensure!(audit_rows == 1, "delete_config 必须落一条成功审计");

        // ---- 再删同一配置：明确 404 ----
        let again = dispatch(
            &app,
            "feishu.approval",
            "delete_config",
            json!({ "config_id": config_id }),
            &[("authorization", operator.as_str())],
        )
        .await;
        ensure!(
            matches!(&again, Err(BaseError::RecordNotFound(_))),
            "重复删除必须明确 404，实际: {again:?}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "配置 CRUD 闭环").await;
}

/// delete_config 同事务清 `pending` 任务行；backfilled / terminal 保留作流水。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn delete_config_clears_pending_tasks_and_keeps_finished_ones() {
    let outcome = async {
        let (database, app, token) = prepare_app(None, None).await?;
        let config_id = seed_config(&database, "appTasks", "tblTasks", true).await?;
        seed_task(&database, config_id, "recPending", "pending").await?;
        seed_task(&database, config_id, "recBackfilled", "backfilled").await?;
        seed_task(&database, config_id, "recTerminal", "terminal").await?;

        let deleted = dispatch_as_operator(
            &app,
            "delete_config",
            &token,
            json!({ "config_id": config_id }),
        )
        .await?;
        ensure!(deleted.code == 0, "delete_config 必须成功: {deleted:?}");
        let data = response_data(&deleted)?;
        ensure!(
            data["deleted_pending_tasks"] == json!(1),
            "只应清 pending 任务: {data}"
        );

        let remaining: Vec<String> =
            sqlx::query_scalar("SELECT `state` FROM `feishu_approval_task` ORDER BY `id`")
                .fetch_all(database.pool())
                .await
                .context("读取剩余任务失败")?;
        ensure!(
            remaining == ["backfilled".to_string(), "terminal".to_string()],
            "backfilled / terminal 必须保留作流水，实际 {remaining:?}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "删除清 pending 保留完结").await;
}

/// list_requests 随行带请求体/返回体；list_tasks 按 config_id / state 过滤。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn list_requests_and_list_tasks_return_rows_with_filters() {
    let outcome = async {
        let (database, app, token) = prepare_app(None, None).await?;
        let config_a = seed_config(&database, "appReqA", "tblReqA", true).await?;
        let config_b = seed_config(&database, "appReqB", "tblReqB", true).await?;
        seed_request_log(&database, "appReqA", "succeeded", "已创建审批实例").await?;
        seed_request_log(&database, "appReqA", "failed", "参数无效").await?;
        seed_task(&database, config_a, "recA1", "pending").await?;
        seed_task(&database, config_a, "recA2", "backfilled").await?;
        seed_task(&database, config_b, "recB1", "pending").await?;

        // ---- list_requests：两条都要回，体量小直接展开 ----
        let listed = dispatch_as_operator(
            &app,
            "list_requests",
            &token,
            json!({ "count_total": true }),
        )
        .await?;
        ensure!(listed.code == 0, "list_requests 必须成功: {listed:?}");
        let data = response_data(&listed)?;
        let items = data["items"].as_array().context("items 必须是数组")?;
        ensure!(items.len() == 2, "应回两条请求记录，实际 {}", items.len());
        ensure!(
            data["total"] == json!(2),
            "count_total 要真的数出来: {data}"
        );
        for item in items {
            ensure!(
                item["request_body"].as_str().is_some(),
                "request_body 必须随行返回: {item}"
            );
            ensure!(
                item["outcome"].as_str().is_some() && item["message"].as_str().is_some(),
                "outcome/message 必须随行返回: {item}"
            );
        }

        // ---- list_tasks：按 config_id 过滤 ----
        let filtered = dispatch_as_operator(
            &app,
            "list_tasks",
            &token,
            json!({
                "where": { "type": "eq", "field": "config_id", "value": config_a },
                "count_total": true,
            }),
        )
        .await?;
        ensure!(filtered.code == 0, "list_tasks 必须成功: {filtered:?}");
        let data = response_data(&filtered)?;
        let items = data["items"].as_array().context("items 必须是数组")?;
        ensure!(items.len() == 2, "A 配置应有两条任务，实际 {}", items.len());
        ensure!(
            items
                .iter()
                .all(|item| item["config_id"] == json!(config_a)),
            "过滤后不得混入别的配置: {data}"
        );

        // ---- list_tasks：再按 state 过滤 ----
        let pending = dispatch_as_operator(
            &app,
            "list_tasks",
            &token,
            json!({
                "where": {
                    "type": "and",
                    "conditions": [
                        { "type": "eq", "field": "config_id", "value": config_a },
                        { "type": "eq", "field": "state", "value": "pending" },
                    ],
                }
            }),
        )
        .await?;
        ensure!(
            pending.code == 0,
            "list_tasks 状态过滤必须成功: {pending:?}"
        );
        let data = response_data(&pending)?;
        let items = data["items"].as_array().context("items 必须是数组")?;
        ensure!(
            items.len() == 1,
            "A 配置只有一条 pending，实际 {}",
            items.len()
        );
        ensure!(
            items[0]["state"] == json!("pending") && items[0]["record_id"] == json!("recA1"),
            "状态过滤结果不对: {data}"
        );

        Ok(())
    }
    .await;

    finish(outcome, "记录与任务列表").await;
}

/// 每个用例的统一收尾：无论成败都清掉测试表。
async fn finish(outcome: anyhow::Result<()>, what: &str) {
    let cleanup = match connect_database().await {
        Ok(database) => reset_database(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("{what} 集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}
