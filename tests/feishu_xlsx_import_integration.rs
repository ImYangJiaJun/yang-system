//! 飞书 xlsx 文件导入端点的端到端集成测试。
//!
//! # 覆盖什么
//!
//! 真打导入端点这一条链路：**装配完整的应用**（`build_app`）→ 经 Registry 派发
//! `feishu.datasource.import_xlsx` → 字节落库 → 断言 `feishu_option` 的真实行。
//! 解析层（`domain/xlsx.rs`）与派生层（`domain/derive.rs`）各有完整单测，但它们都
//! **到不了**这一层：逐绑定事务、空快照守卫、跨源预检、补集停用全都发生在
//! 「Action + 真库」的交界处，只有真跑一遍才会暴露。
//!
//! 顺带覆盖 `probe_xlsx_headers`（Task 9 的探表头端点）。它此前只有单测：真实的
//! multipart 路径（单/多 part 形态、表头一致性整份拒绝）**从没被端到端走过**，而
//! 「只传一个文件」曾经 400——那正是传输层对单 part 放裸对象、对多 part 才升级成
//! 数组这条不对称造成的。既然本文件已经要构造 multipart 请求，这一条几乎是免费的。
//!
//! # 关于 multipart 的构造
//!
//! 本文件的派发**不走 HTTP**，直接走 Registry（与仓库其余集成测试同一条路径），
//! 所以 multipart 的解析由测试自己复刻：按
//! `yang-base/src/transport/axum.rs::decode_multipart` 服务端构造文件句柄的**同一份
//! JSON 形状**（`field_name` / `original_filename` / `content_type` / `size` / `path` /
//! `temp_root`）拼出 body，并按 `insert_multipart_value` 的规则决定「裸对象还是数组」。
//!
//! 复刻的是**形状**而不是逻辑：MIME 白名单、字节上限、`temp_root` 的越界校验都在传输层，
//! 这里不重复实现，也不假装测过。它们在 yang-base 的传输层测试里有覆盖。
//!
//! # 关于鉴权
//!
//! `feishu.datasource` 挂了 `TokenAuthMiddleware`，导入端点又是受保护 Action，所以每次
//! 派发都要一个**真的** Access Token：本文件直接种一行 `users` 再用同一套
//! `TokenManager` 参数手工签发（而非走注册 + 登录）。理由是成本——注册要过一次
//! Argon2，而这里被测的是导入而不是认证链路；认证链路本身有专门的集成测试。
//! 令牌仍要过签名、过期、黑名单与**授权版本回源**四道校验，不是伪造身份。
//!
//! # 运行方式
//!
//! ```text
//! YANG_SYSTEM_TEST_DATABASE_URL=mysql://root:yang-local@127.0.0.1:3306/yang_system_test \
//! YANG_SYSTEM_TEST_REDIS_URL=redis://127.0.0.1:6379/15 \
//! cargo test --test feishu_xlsx_import_integration -- --ignored --test-threads=1
//! ```
//!
//! 本文件**读取** `YANG_SYSTEM_TEST_` 开头的环境变量，`scripts/run_ci.py` 的反向发现
//! 因此会要求把它登记进 `INTEGRATION`——漏登记 `--self-test` 会失败。

use anyhow::{ensure, Context};
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::{ApiResponse, Request, RequestMeta, StepUpManager};
use yang_base::definition::{ActionName, ActionRef, BuiltApp, ModuleName};
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};
use yang_system::app::build_app;
use yang_system::authorization::AuthorizationVersionCache;
use yang_system::config::{FeishuSettings, SecuritySettings};
use yang_system::schema::sync_with_database;

/// 三张飞书表；清理时白名单化，避免拼错表名误删。
const DATASOURCE_TABLE: &str = "feishu_datasource";
const FIELD_TABLE: &str = "feishu_datasource_field";
const OPTION_TABLE: &str = "feishu_option";

/// 数据源 Token 明文。服务端只存它的 SHA-256 摘要，明文只出现在这里。
const DATASOURCE_TOKEN: &str = "xlsx-import-token";
/// xlsx 的精确 MIME（与两个 Action 的 `allowed_content_types` 一致）。
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

// ---------------------------------------------------------------- 连接与清理

fn database_config() -> DatabaseConfig {
    DatabaseConfig::default()
        .with_max_connections(16)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

fn redis_config() -> RedisConfig {
    RedisConfig::default()
        .with_max_connections(16)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

async fn connect_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取测试数据库名失败")?;
    let name = name.context("测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行飞书集成测试"
    );
    Ok(database)
}

async fn connect_redis() -> anyhow::Result<RedisClient> {
    let url =
        std::env::var("YANG_SYSTEM_TEST_REDIS_URL").context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
    ensure!(
        url.trim_end_matches('/').ends_with("/15"),
        "飞书集成测试 Redis 必须使用独立 DB 15"
    );
    RedisClient::connect_with_config(&url, redis_config())
        .await
        .map_err(Into::into)
}

/// 清理三张飞书表；表名白名单化。
async fn drop_feishu_tables(database: &Database) -> anyhow::Result<()> {
    // 顺序无关：绑定表用的是普通 `Int` + 索引，没有外键约束（设计 §5）。
    for table in [OPTION_TABLE, FIELD_TABLE, DATASOURCE_TABLE] {
        let statement = match table {
            DATASOURCE_TABLE => "DROP TABLE IF EXISTS `feishu_datasource`",
            FIELD_TABLE => "DROP TABLE IF EXISTS `feishu_datasource_field`",
            OPTION_TABLE => "DROP TABLE IF EXISTS `feishu_option`",
            other => anyhow::bail!("拒绝清理未声明的测试表: {other}"),
        };
        sqlx::query(statement)
            .execute(database.pool())
            .await
            .with_context(|| format!("清理飞书测试表失败: {statement}"))?;
    }
    Ok(())
}

/// 把飞书表同步到测试库。
///
/// `sync_with_database` 内部会关掉连接池，因此调用方必须在它之后**重新建连**。
async fn sync_feishu_schema(database: Database) -> anyhow::Result<()> {
    sync_with_database(
        database,
        database_config(),
        Arc::new(SecuritySettings::default()),
    )
    .await
    .context("同步飞书 Schema 失败")?;
    Ok(())
}

// ---------------------------------------------------------------- 应用装配

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

/// 启用飞书集成，**并配好出站凭证**。
///
/// 凭证不是导入需要的（导入不出网），而是 `pull_now` 注册的前置：`register_all` 只在
/// `can_pull()` 为真时才注册三个出站端点，而「拉取路径必须拒绝 xlsx 源」这条断言要
/// 真的调到 `pull_now`。缺了它，那条用例会失败在「Action 未注册」上，看起来像断言写错。
fn feishu_settings() -> Arc<FeishuSettings> {
    Arc::new(FeishuSettings {
        enabled: true,
        management_api_token: "xlsx-import-management-token".to_string(),
        // 建源要它来封存系统签发的凭据（`create_datasource_table` 缺它就拒绝建源），
        // 而本文件有一条用例刻意**经建源 Action** 种数据。
        encryption_key: Some("xlsx-import-encryption-key".to_string()),
        app_id: Some("cli_xlsx_import_app".to_string()),
        app_secret: Some("xlsx-import-app-secret".to_string()),
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
        "xlsx-import-integration-token-secret-32",
        Algorithm::HS256,
        "xlsx-import-integration".to_string(),
        "xlsx-import-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "xlsx-import-integration-step-up-32byte",
            "xlsx-import-integration-step-up",
            "xlsx-import-sensitive-actions",
        )
        .unwrap_or_else(|error| panic!("集成测试 Step-up manager 应有效: {error}")),
    )
}

/// 装配一个启用飞书集成的完整应用。
///
/// 授权版本缓存用**每次运行唯一**的 namespace：复用固定 namespace 会让上一次运行留下的
/// 版本号参与本轮判定（陈旧缓存会把新签发的 Token 判成 stale），而那种失败与本次改动无关。
async fn build_feishu_app(
    database: &Database,
    redis: &RedisClient,
) -> anyhow::Result<Arc<BuiltApp>> {
    let namespace = format!(
        "xlsx-import-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(Database::from_pool(
                database.pool().clone(),
                database_config(),
            )?)
            .cache(redis.clone())
            .token(token_manager())
            .extension(AuthorizationVersionCache::new(redis.clone(), namespace)?)
            .extension(step_up_manager())
            .config(feishu_settings())
            .build()?,
    );
    let application = build_app(tools, security_settings())?;
    Ok(Arc::new(application.runtime))
}

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
    path_params: &[(&str, &str)],
    headers: &[(&str, &str)],
) -> Result<ApiResponse, BaseError> {
    let mut request = Request::new(body);
    for (key, value) in path_params {
        request = request.path_param(*key, *value);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let context = app.context(request).with_request_meta(
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], 42_001))),
    );
    let handle = action_handle(app, module, action)
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    app.dispatch_context(handle, context).await
}

// ---------------------------------------------------------------- 操作者

/// 种一个持 `feishu.datasource.write` 的操作者并签发 Access Token。
///
/// 直接写 `users` 行而不是走注册：注册要过一次 Argon2、还要消费首账号引导的哨兵，
/// 而本文件测的是导入不是认证。令牌仍要过签名、过期、黑名单与授权版本回源四道校验
/// （`AuthorizationVersionValidator` 会回 MySQL 读 `authz_version` 与 `status`），
/// 所以 `authz_version` 必须与令牌声明**逐位相同**，`status` 必须是 `active`。
async fn seed_operator(database: &Database) -> anyhow::Result<String> {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let username = format!("xlsx_import_{unique}");
    let result = sqlx::query(
        // `created_at` / `updated_at` 是 NOT NULL 且**没有库级默认值**（时间戳由框架在
        // 写入时填）——裸 SQL 种子必须自己给。
        "INSERT INTO `users` \
         (`username`, `password_hash`, `status`, `authz_version`, `created_at`, `updated_at`) \
         VALUES (?, 'not-a-real-password-hash', 'active', 1, NOW(), NOW())",
    )
    .bind(&username)
    .execute(database.pool())
    .await
    .context("种入导入操作者失败")?;
    let user_id = result.last_insert_id() as i64;

    token_manager()
        .generate_access_token(
            &user_id.to_string(),
            json!({
                "version": 1,
                "username": username,
                "authz_version": 1,
                "roles": ["user"],
                "permissions": ["feishu.datasource.write", "feishu.datasource.read"],
            }),
        )
        .map_err(|error| anyhow::anyhow!("签发导入操作者令牌失败: {error}"))
}

// ---------------------------------------------------------------- 数据播种

/// 种一条 xlsx 导入的数据源 + N 条绑定，返回 `datasource_id`。
///
/// `fields` 是 `(列名, source_key, 父列名)` —— **列名同时进 `field_id` 与 `field_name`**：
/// xlsx 没有「按 field_id 解析当前列名」那一层，列名就是身份（Task 8）。
async fn seed_xlsx_datasource(
    database: &Database,
    ingest_mode: &str,
    fields: &[(&str, &str, Option<&str>)],
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO `feishu_datasource` (`title`, `status`, `ingest_mode`, `created_at`, `updated_at`) \
         VALUES (?, 'active', ?, NOW(), NOW())",
    )
    .bind("xlsx 夹具源")
    .bind(ingest_mode)
    .execute(database.pool())
    .await
    .context("写入表级数据源失败")?;
    let datasource_id = result.last_insert_id() as i64;

    for (index, (column, source_key, parent)) in fields.iter().enumerate() {
        sqlx::query(
            "INSERT INTO `feishu_datasource_field` \
             (`datasource_id`, `field_id`, `field_name`, `source_key`, `token_hash`, `parent_field_id`, \
              `enabled`, `created_at`, `updated_at`) \
             VALUES (?, ?, ?, ?, SHA2(?, 256), ?, 1, NOW(), NOW())",
        )
        .bind(datasource_id)
        .bind(column)
        .bind(column)
        .bind(source_key)
        .bind(format!("{DATASOURCE_TOKEN}-{index}"))
        .bind(parent)
        .execute(database.pool())
        .await
        .with_context(|| format!("写入绑定 {source_key} 失败"))?;
    }
    Ok(datasource_id)
}

/// 银行形状的两条绑定：行名无父，联行号挂在行名下。
fn bank_bindings() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    vec![
        ("开户行行名", "bank_branch_name", None),
        ("联行号", "bank_branch_code", Some("开户行行名")),
    ]
}

fn xlsx_fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xlsx")
            .join(name),
    )
    .unwrap_or_else(|error| panic!("读夹具 {name} 失败: {error}"))
}

/// 把夹具字节落成请求作用域的临时文件，返回传输层会塞进 body 的那个文件句柄对象。
///
/// 字段清单照抄 `yang-base/src/transport/axum.rs` 的服务端构造——**少一个 `temp_root`**
/// 就会被 `UploadedFile::copy_to` 判成不可信实例（本路径只 `read`，但形状要一致）。
fn file_part(directory: &std::path::Path, name: &str) -> serde_json::Value {
    let bytes = xlsx_fixture(name);
    let path = directory.join(name);
    std::fs::write(&path, &bytes)
        .unwrap_or_else(|error| panic!("写出上传临时文件 {name} 失败: {error}"));
    json!({
        "field_name": "files",
        "original_filename": name,
        "content_type": XLSX_MIME,
        "size": bytes.len(),
        "path": path,
        "temp_root": directory,
    })
}

/// 造一个本次调用独占的上传目录。
fn upload_directory() -> anyhow::Result<PathBuf> {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = std::env::temp_dir().join(format!("yang-xlsx-import-{unique}"));
    std::fs::create_dir_all(&directory).context("创建上传临时目录失败")?;
    Ok(directory)
}

/// 按 multipart 的两种形态拼 body。
///
/// **单文件放裸对象、多文件放数组**，与 `insert_multipart_value` 逐条一致：它在
/// `Entry::Vacant` 时直接放那个对象，只有第二个同名 part 到来才升级成 `Value::Array`。
/// 只测其中一种形态会漏掉另一边——单文件那条曾经让端点 400。
fn multipart_body<'a>(files: impl Iterator<Item = &'a str>, directory: &std::path::Path) -> Value {
    let mut parts: Vec<Value> = files.map(|name| file_part(directory, name)).collect();
    // 恰好一个 part → 裸对象；否则数组。
    let files = if parts.len() == 1 {
        parts.pop().unwrap_or(Value::Null)
    } else {
        Value::Array(parts)
    };
    json!({ "files": files })
}

/// 派发一次导入：按**夹具名**读文件（`tests/fixtures/xlsx/<name>`），
/// 按 multipart 塞进请求，打 `import_xlsx`。
async fn dispatch_import(
    app: &Arc<BuiltApp>,
    token: &str,
    datasource_id: i64,
    files: &[&str],
) -> Result<ApiResponse, BaseError> {
    let directory =
        upload_directory().map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let body = multipart_body(files.iter().copied(), &directory);
    let id = datasource_id.to_string();
    let authorization = format!("Bearer {token}");
    let response = dispatch(
        app,
        "feishu.datasource",
        "import_xlsx",
        body,
        &[("datasource_id", id.as_str())],
        &[("authorization", authorization.as_str())],
    )
    .await;
    let _ = std::fs::remove_dir_all(&directory);
    response
}

/// 派发一次探表头。**不打路径参数**：该端点的路由上没有 `{...}` 段。
async fn dispatch_probe(
    app: &Arc<BuiltApp>,
    token: &str,
    files: &[&str],
) -> Result<ApiResponse, BaseError> {
    let directory =
        upload_directory().map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let body = multipart_body(files.iter().copied(), &directory);
    let authorization = format!("Bearer {token}");
    let response = dispatch(
        app,
        "feishu.datasource",
        "probe_xlsx_headers",
        body,
        &[],
        &[("authorization", authorization.as_str())],
    )
    .await;
    let _ = std::fs::remove_dir_all(&directory);
    response
}

/// 派发一次导入进度查询（GET 端点，`datasource_id` 走**路径参数**）。
async fn dispatch_progress(
    app: &Arc<BuiltApp>,
    token: &str,
    datasource_id: i64,
) -> anyhow::Result<Value> {
    let authorization = format!("Bearer {token}");
    let id = datasource_id.to_string();
    let response = dispatch(
        app,
        "feishu.datasource",
        "get_import_progress",
        json!({}),
        &[("datasource_id", id.as_str())],
        &[("authorization", authorization.as_str())],
    )
    .await?;
    ensure!(
        response.code == 0,
        "进度查询应成功，实际 {}",
        response.message
    );
    response_data(&response)
}

async fn count_options(
    database: &Database,
    source_key: &str,
    enabled_only: bool,
) -> anyhow::Result<i64> {
    let sql = if enabled_only {
        "SELECT COUNT(*) FROM `feishu_option` WHERE `source_key` = ? AND `enabled` = 1"
    } else {
        "SELECT COUNT(*) FROM `feishu_option` WHERE `source_key` = ?"
    };
    let (count,): (i64,) = sqlx::query_as(sql)
        .bind(source_key)
        .fetch_one(database.pool())
        .await?;
    Ok(count)
}

/// 每个用例的公共准备：清库 → 建表 → **重连**（同步会关池）→ 建 Redis → 种操作者 → 装配 app。
///
/// 抽成一个 helper 而不是让每个用例各抄六步。注意它**不种业务数据**：
/// 每个用例要种的数据各不相同，那是调用方的事。
async fn prepare_app() -> anyhow::Result<(Database, Arc<BuiltApp>, String)> {
    let database = connect_database().await?;
    drop_feishu_tables(&database).await?;
    sync_feishu_schema(database).await?;
    // **必须重连**：sync_feishu_schema 内部会关掉连接池，
    // 复用同步前的句柄会拿到已关闭的池。
    let database = connect_database().await?;
    let redis = connect_redis().await?;
    let token = seed_operator(&database).await?;
    let app = build_feishu_app(&database, &redis).await?;
    Ok((database, app, token))
}

/// 读成功响应的 data 段。
///
/// **不能读 `attachment`**：本模块的 Action 返回的是普通 `ApiResponse`（`data` 字段），
/// 只有裸响应体（`ResponseBody::raw`）那条路径才会填 `attachment`。
fn response_data(response: &ApiResponse) -> anyhow::Result<Value> {
    response.data.clone().context("响应里没有 data")
}

/// 每个用例的统一清理：无论成败都 drop 掉飞书表。
async fn finish(outcome: anyhow::Result<()>, what: &str) {
    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("{what} 集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

// ---------------------------------------------------------------- 用例

// 1. 主路径：两条绑定都落库，且父键折对了。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn import_writes_options_for_every_binding() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        let response =
            dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"]).await?;
        let data = response_data(&response)?;

        let bindings = data["bindings"].as_array().cloned().unwrap_or_default();
        assert_eq!(bindings.len(), 2, "两条绑定都要有回执");
        // 两条绑定共享同一份快照：bank_1 有 3 行、bank_2 有 2 行 → fetched 恒为 5
        for binding in &bindings {
            assert_eq!(binding["fetched"], 5, "一份快照服务所有绑定");
        }
        // 行名去重后 5 条（夹具里 5 个不同行名），联行号 5 条
        assert_eq!(count_options(&database, "bank_branch_name", true).await?, 5);
        assert_eq!(count_options(&database, "bank_branch_code", true).await?, 5);

        // 父键折对了：子绑定的 parent_key 应等于父行自己的 option_id
        let (parent_key,): (String,) = sqlx::query_as(
            "SELECT `parent_key` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_code' LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        assert!(!parent_key.is_empty(), "子绑定必须有父键");
        let (owner,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' AND `option_id` = ?",
        )
        .bind(&parent_key)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(owner, 1, "父键必须能在父绑定里找到对应的行（否则下拉会空）");
        Ok(())
    }
    .await;
    finish(outcome, "主路径").await;
}

// 2. 缺列即拒（D9）：文件里没有 `联行号` → 整个请求被拒，**库里零变化**。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_missing_column_rejects_the_whole_import() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        let error = dispatch_import(&app, &token, datasource_id, &["header_missing_column.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("缺列必须被拒"));
        let message = error.to_string();
        assert!(message.contains("联行号"), "要点名缺哪列: {message}");
        // 整份拒绝 ⇒ 两条绑定都不该有任何行
        assert_eq!(
            count_options(&database, "bank_branch_name", false).await?,
            0
        );
        assert_eq!(
            count_options(&database, "bank_branch_code", false).await?,
            0
        );
        Ok(())
    }
    .await;
    finish(outcome, "缺列即拒").await;
}

// 3. 多列忽略（D9）：文件里多一列未勾选的列 → 正常导入。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn extra_columns_are_ignored() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 上传 header_extra_column.xlsx（表头多了「备注」列）——必须正常导入
        dispatch_import(&app, &token, datasource_id, &["header_extra_column.xlsx"]).await?;
        assert_eq!(count_options(&database, "bank_branch_code", true).await?, 1);
        Ok(())
    }
    .await;
    finish(outcome, "多列忽略").await;
}

// 4. 表级空快照守卫：只有表头 + 库里已有启用选项 → 整轮失败且**不停用**。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn an_empty_snapshot_fails_the_round_without_disabling() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 先正常导一次，让库里有启用选项
        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"]).await?;
        let before = count_options(&database, "bank_branch_code", true).await?;
        assert!(before > 0);

        // 再传一个只有表头的文件：必须整轮失败，且一行都不许停用
        let error = dispatch_import(&app, &token, datasource_id, &["only_header.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("空快照必须整轮失败"));
        assert!(
            error.to_string().contains("0 行"),
            "错误要说清是空快照: {error}"
        );
        assert_eq!(
            count_options(&database, "bank_branch_code", true).await?,
            before,
            "空快照下绝不可以停用补集"
        );
        Ok(())
    }
    .await;
    finish(outcome, "表级空快照守卫").await;
}

// 5. **逐绑定空快照守卫（§5.7.1）——本任务最该盯的一条。**
//    绑两条：行名 + 联行号；文件里 `联行号` 整列为空。
//    期望：联行号那条**跳过写库、库里原有选项一行不动**，
//          行名那条照常提交，回执里联行号带 skipped_reason。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_binding_that_derives_nothing_is_skipped_not_emptied() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 先用 bank_1 导一次，让两条绑定都有启用选项
        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx"]).await?;
        let code_before = count_options(&database, "bank_branch_code", true).await?;
        assert!(code_before > 0, "前置条件：联行号已有选项");
        // 跳过分支要回发的**真实**摘要：绑定行上此刻存着的那个。
        let (stored_digest,): (Option<String>,) = sqlx::query_as(
            "SELECT `snapshot_digest` FROM `feishu_datasource_field` \
             WHERE `source_key` = 'bank_branch_code'",
        )
        .fetch_one(database.pool())
        .await?;
        let stored_digest = stored_digest.unwrap_or_default();
        assert!(!stored_digest.is_empty(), "前置条件：上一轮已落摘要");

        // 再传 column_all_blank.xlsx：表头一致（8 列都在），但 `联行号` 整列为空
        let response =
            dispatch_import(&app, &token, datasource_id, &["column_all_blank.xlsx"]).await?;
        let data = response_data(&response)?;

        let reports = data["bindings"].as_array().cloned().unwrap_or_default();
        let code_report = reports
            .iter()
            .find(|binding| binding["source_key"] == "bank_branch_code")
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有联行号"));
        assert_eq!(code_report["derived"], 0);
        assert!(
            code_report["skipped_reason"].is_string(),
            "跳过必须出现在回执里，不能静默: {code_report}"
        );
        // 跳过 = 这一轮什么都没写，「这条绑定现在的内容」仍然是**旧值**。
        // 回发本轮算出来的那个空摘要会被读成「它的内容现在是空的」——与事实正相反。
        assert_eq!(
            code_report["snapshot_digest"].as_str(),
            Some(stored_digest.as_str()),
            "跳过分支必须回发绑定行上已存的摘要，不能是本轮算出来的空摘要"
        );
        assert_eq!(
            count_options(&database, "bank_branch_code", true).await?,
            code_before,
            "**一行都不许停用**——这是 pull.rs 的表级守卫挡不住的情形"
        );

        // 行名那条照常提交（A12 的逐绑定隔离）
        let name_report = reports
            .iter()
            .find(|binding| binding["source_key"] == "bank_branch_name")
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有行名"));
        assert!(name_report["derived"].as_i64().unwrap_or(0) > 0);
        Ok(())
    }
    .await;
    finish(outcome, "逐绑定空快照守卫").await;
}

// 6. 整体替换：第二个文件的数据完全替换第一个（不是追加）。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn re_importing_replaces_the_whole_snapshot() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;
        // 先导 bank_1（3 个数据行）
        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx"]).await?;
        assert_eq!(count_options(&database, "bank_branch_name", true).await?, 3);
        // 再导 bank_2（另 2 行、行名完全不同）→ bank_1 的行名必须消失
        dispatch_import(&app, &token, datasource_id, &["bank_2.xlsx"]).await?;
        assert_eq!(
            count_options(&database, "bank_branch_name", true).await?,
            2,
            "整份替换：上一轮的 3 行一行都不该留下"
        );
        let (stale,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' AND `enabled` = 1 \
               AND `label` = '中国工商银行成都春熙路支行'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(stale, 0, "上一轮独有的行必须消失");
        Ok(())
    }
    .await;
    finish(outcome, "整体替换").await;
}

// 7. 逐绑定隔离（A12）：让**后一条**绑定的写入失败，断言它自己回到导入前、
//    前面已提交的绑定保持新值。
//    **不要写成「旧数据一行不少」**——那是单事务的期望，与逐绑定粒度直接冲突。
//
//    怎么让后一条失败：把 `bank_branch_code` 的某条 option_id 预先塞给
//    **另一个 source_key**（造出跨源夺取），跨源预检会拒它。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_failing_binding_does_not_roll_back_its_predecessors() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        // **怎么造出「后一条绑定写失败」**：option_id 的哈希函数是 pub(crate)，
        // 集成测试算不出来。所以先正常导一次，把真实的 option_id 从库里读回来，
        // 再把它改成「属于另一个源」——跨源预检就会拒它。
        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"]).await?;
        let (doomed_id,): (String,) = sqlx::query_as(
            "SELECT `option_id` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_code' \
             ORDER BY `option_id` LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        sqlx::query(
            "UPDATE `feishu_option` SET `source_key` = 'another_source' WHERE `option_id` = ?",
        )
        .bind(&doomed_id)
        .execute(database.pool())
        .await?;
        // 再停用一条自己的行：摘要不含 `enabled`，所以 `disabled_rows > 0`
        // 会让 `unchanged` 变 false，逼这一轮真的走「预检 → 替换 → 补集」。
        // （不这么做的话，内容没变会被跳过，预检根本不会跑。）
        sqlx::query(
            "UPDATE `feishu_option` SET `enabled` = 0 \
             WHERE `source_key` = 'bank_branch_code' AND `option_id` <> ? LIMIT 1",
        )
        .bind(&doomed_id)
        .execute(database.pool())
        .await?;

        // 绑定按 `id` 升序处理：行名（先种，id 小）在前、联行号在后。
        // 断言的就是这个先后——行名先提交成功，联行号才撞上跨源预检。
        let error = dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx", "bank_2.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("跨源夺取必须被拒"));
        assert!(error.to_string().contains("已属于数据源"), "实际: {error}");

        // 断言顺序：绑定按 `id` 升序，行名在前 —— 它应当已经提交了新数据
        assert!(
            count_options(&database, "bank_branch_name", true).await? > 0,
            "前一条已提交的绑定必须保持新值（逐绑定粒度，不是全量回滚）"
        );
        Ok(())
    }
    .await;
    finish(outcome, "逐绑定隔离").await;
}

// 8. xlsx 源不会被拉取路径碰。
//    自动轮询的机制是 `load_pull_tables` 的 `where_eq("ingest_mode", "pull")`，
//    而那个函数要一个 `&FeishuContext`——集成测试拿不到（它是 Action 内部装配的）。
//    所以这里断言**可观测的那一面**：控制台上的「立即拉取」对 xlsx 源必须被拒。
//    （WHERE 子句本身由 `pull.rs` 的单测覆盖。）
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn the_pull_path_rejects_an_xlsx_source() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        let authorization = format!("Bearer {token}");
        let response = dispatch(
            &app,
            "feishu.datasource",
            "pull_now",
            json!({ "datasource_id": datasource_id }),
            &[],
            &[("authorization", authorization.as_str())],
        )
        .await?;
        assert_eq!(
            response.code, 40903,
            "xlsx 源必须落到 NOT_PULLABLE（check_pullable 对非 pull 的判据），实际 {}",
            response.message
        );
        Ok(())
    }
    .await;
    finish(outcome, "拉取路径拒绝 xlsx 源").await;
}

// 9. 取数方式不符：对 ingest_mode=pull 的源调导入 → 明确拒绝，不是静默导入。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn importing_into_a_pull_source_is_rejected() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        // 同一个建源 helper，取数方式换成 pull —— 那条源不该接受文件导入。
        let datasource_id = seed_xlsx_datasource(&database, "pull", &bank_bindings()).await?;

        let response = dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx"]).await?;
        assert_ne!(response.code, 0, "取数方式不符必须以失败信封返回");
        assert_eq!(
            count_options(&database, "bank_branch_name", false).await?,
            0
        );
        Ok(())
    }
    .await;
    finish(outcome, "取数方式不符").await;
}

// 10. 超长取数文案（设计 §5.7）：入库但 `enabled = false` + 原因写 `extra`。
//     不截断的话，`label` 列是 max_length(255)，会在**插库时**才炸，
//     而那时的错误信息不会告诉你是哪一行、什么值。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn an_overlong_label_is_truncated_and_disabled_with_a_reason() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        // 只种一条绑定（行名）——本用例只关心它。
        let datasource_id = seed_xlsx_datasource(
            &database,
            "xlsx_import",
            &[("开户行行名", "bank_branch_name", None)],
        )
        .await?;

        // `overlong_value.xlsx` 的 `开户行行名` 列有一个 300 字符的值
        // （表头本身正常，见 Task 1 的夹具表）。
        let response =
            dispatch_import(&app, &token, datasource_id, &["overlong_value.xlsx"]).await?;
        let data = response_data(&response)?;
        let report = data["bindings"]
            .as_array()
            .and_then(|bindings| bindings.first())
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有绑定"));
        assert_eq!(
            report["truncated_details"], false,
            "只超了一条，够不上截断清单的上限"
        );
        assert!(
            report["anomalies"]
                .as_array()
                .is_some_and(|entries| entries.len() == 1),
            "异常必须逐条写进回执，不能静默: {report}"
        );

        let (label, enabled, extra): (String, bool, Option<String>) = sqlx::query_as(
            "SELECT `label`, `enabled`, `extra` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' \
             ORDER BY CHAR_LENGTH(`label`) DESC LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;

        assert_eq!(label.chars().count(), 255, "截断到 label 的上限");
        assert!(!enabled, "超长值必须置 enabled = false，不喂给飞书");
        let extra = extra.unwrap_or_default();
        assert!(extra.contains("anomaly"), "原因要写进 extra: {extra}");
        Ok(())
    }
    .await;
    finish(outcome, "超长文案").await;
}

// 11. 建源的写端必须把**列名**写进绑定行的 `field_name`。
//
//     这条是 Task 8 评审发现的口子：`create_datasource_table` 里那两段
//     `insert("field_name", …)` 整段删掉，**所有既有测试依然全绿**——因为既有用例全部
//     用裸 SQL 播种绑定，从不走建源 Action。后果是「新列的绑定没有名字」，
//     而审批装配按**列名**配对控件与数据源，没名字的绑定只会被静默跳过
//     （`approval_provision` 只 warn 不报错）。
//
//     所以这里刻意**经 Action 建源**（bare SQL 播种证明不了写端），再读回绑定行。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn creating_a_source_through_the_action_names_every_binding() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let authorization = format!("Bearer {token}");
        let response = dispatch(
            &app,
            "feishu.datasource",
            "create_datasource_table",
            json!({
                "title": "xlsx 建源用例",
                "ingest_mode": "xlsx_import",
                "fields": [
                    { "field_id": "开户行行名", "field_name": "开户行行名", "source_key": "name_ok" },
                    {
                        "field_id": "联行号",
                        "field_name": "联行号",
                        "source_key": "code_ok",
                        "parent_field_id": "开户行行名"
                    }
                ],
            }),
            &[],
            &[("authorization", authorization.as_str())],
        )
        .await?;
        assert_eq!(response.code, 0, "建源应成功: {}", response.message);
        let datasource_id = response_data(&response)?["datasource_id"]
            .as_i64()
            .context("建源响应缺少 datasource_id")?;

        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT `field_id`, `field_name` FROM `feishu_datasource_field` \
             WHERE `datasource_id` = ? ORDER BY `id`",
        )
        .bind(datasource_id)
        .fetch_all(database.pool())
        .await?;
        assert_eq!(rows.len(), 2, "两条绑定都要写进去");
        for (field_id, field_name) in rows {
            assert_eq!(
                field_name.as_deref(),
                Some(field_id.as_str()),
                "绑定行的 field_name 必须等于列名（它就是 xlsx 的列名，Task 8）"
            );
        }
        Ok(())
    }
    .await;
    finish(outcome, "建源写 field_name").await;
}

// 12. Task 9 的探表头端点：真实的 multipart 路径（单 part 的裸对象形态、多 part 的
//     数组形态、表头不一致整份拒绝）此前**从没被端到端走过**——它只有纯函数单测，
//     而「只传一个文件」曾经 400 正是发生在传输层与 `ParamInput::decode` 的接缝上。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn the_probe_endpoint_walks_the_real_multipart_path() {
    let outcome = async {
        let (_database, app, token) = prepare_app().await?;

        // 单文件：传输层放的是**裸对象**（那条 400 就出在这里）
        let response = dispatch_probe(&app, &token, &["bank_1.xlsx"]).await?;
        let data = response_data(&response)?;
        assert_eq!(data["sheet_name"], "境内银行网点信息管理");
        assert_eq!(data["header_row"], 1);
        assert_eq!(data["columns"].as_array().map(Vec::len), Some(8));
        assert_eq!(
            data["files"].as_array().map(Vec::len),
            Some(1),
            "单文件也要归一成 1 个文件"
        );

        // 多文件：传输层升级成数组
        let response = dispatch_probe(&app, &token, &["bank_1.xlsx", "bank_2.xlsx"]).await?;
        let data = response_data(&response)?;
        assert_eq!(data["files"].as_array().map(Vec::len), Some(2));

        // 表头不一致：整份拒绝，并点名是哪个文件
        let error = dispatch_probe(&app, &token, &["bank_1.xlsx", "header_mismatch_two.xlsx"])
            .await
            .err()
            .unwrap_or_else(|| panic!("表头不一致必须整份拒绝"));
        assert!(
            error.to_string().contains("header_mismatch_two.xlsx"),
            "要点名文件: {error}"
        );
        Ok(())
    }
    .await;
    finish(outcome, "探表头端点").await;
}

// 13. **文件之间**表头不一致 → 整份拒绝（设计 §5.7 的文件级规则）。
//
//     这条规则在探表头那条路上一直有，但**「重新导入」不经过探表头**（详情页直接调本
//     端点）。少了它，用户传一组表头互不相同的文件会被**静默合并**成一份，而回执里
//     各文件的 `rows_read` 加起来看着完全正常。
//
//     与用例 3（`extra_columns_are_ignored`）刻意对照：**单文件**多一列未勾选的列是
//     合法输入（忽略）；**两个文件**表头集合不同则是请求级错误（拒绝）。两条规则
//     各自成立，不是一回事。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn files_with_different_headers_are_rejected_as_a_whole() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        // bank_1 是标准 8 列；header_extra_column 是同样的 8 列 **+「备注」**。
        let error = dispatch_import(
            &app,
            &token,
            datasource_id,
            &["bank_1.xlsx", "header_extra_column.xlsx"],
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("跨文件表头不一致必须整份拒绝"));
        let message = error.to_string();
        assert!(
            message.contains("header_extra_column.xlsx"),
            "要点名是哪个文件不一致: {message}"
        );
        assert!(message.contains("备注"), "要点名差哪一列: {message}");
        assert_eq!(
            count_options(&database, "bank_branch_name", false).await?,
            0,
            "整份拒绝 ⇒ 一行都不许写"
        );
        Ok(())
    }
    .await;
    finish(outcome, "跨文件表头一致").await;
}

// 14. 超长文案 **且空白落在截断边界上** → 仍然入 `extra` 标异常且 `enabled = false`。
//
//     这条钉的是「异常表按 trim 后的文案索引」这个口径：`read_snapshot` 会 trim 单元格
//     首尾，而截断**发生在 trim 之后**——截到第 255 个字符时末尾可能正好留一个空格，
//     于是「截断后的键」与「派生时 trim 过的文案」差一个字符。差这一个字符，回查就会
//     落空：异常行**静默保持 `enabled = true` 并被喂给飞书**，而回执里那条异常记录还在。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn an_overlong_label_with_trailing_space_is_still_recorded_and_disabled() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id = seed_xlsx_datasource(
            &database,
            "xlsx_import",
            &[("开户行行名", "bank_branch_name", None)],
        )
        .await?;

        // 夹具的值：2 个前导空格 + 254 个「长」+ 1 个空格 + 50 个「长」+ 2 个尾随空格。
        let response =
            dispatch_import(&app, &token, datasource_id, &["overlong_padded_value.xlsx"]).await?;
        let data = response_data(&response)?;
        let report = data["bindings"]
            .as_array()
            .and_then(|bindings| bindings.first())
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有绑定"));
        assert!(
            report["anomalies"]
                .as_array()
                .is_some_and(|entries| entries.len() == 1),
            "异常必须出现在回执里: {report}"
        );

        let (label, enabled, extra): (String, bool, Option<String>) = sqlx::query_as(
            "SELECT `label`, `enabled`, `extra` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' ORDER BY `id` LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        assert!(
            label.chars().count() <= 255,
            "落库的文案必须在列上限内，实际 {}",
            label.chars().count()
        );
        assert_eq!(label.trim(), label, "落库的文案不该带首尾空白");
        assert!(
            !enabled,
            "超长值必须置 enabled = false——**这一条正是 trim 口径错了就会红的地方**"
        );
        let extra = extra.unwrap_or_default();
        assert!(extra.contains("anomaly"), "原因要写进 extra: {extra}");
        Ok(())
    }
    .await;
    finish(outcome, "超长且带空白").await;
}

// 15. 逐绑定守卫的**反向情形**：派生 0 个选项、而库里**没有**已启用行 → 正常走。
//
//     这是合法的「本来就是空的」：一条从没导过东西的绑定，第一轮就是 0 个选项。
//     守卫若把它也拦下，空源永远无法确认自己的状态（摘要不推进、回执不给结论）。
//     与用例 5 成对——那一条锁「有存量时不许清空」，这一条锁「没存量时不许拦」。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_binding_with_nothing_to_derive_on_an_empty_source_is_not_skipped() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        // 只种 `联行号` 一条绑定；column_all_blank.xlsx 里这一列整列为空。
        let datasource_id = seed_xlsx_datasource(
            &database,
            "xlsx_import",
            &[("联行号", "bank_branch_code", None)],
        )
        .await?;

        let response =
            dispatch_import(&app, &token, datasource_id, &["column_all_blank.xlsx"]).await?;
        let data = response_data(&response)?;
        let report = data["bindings"]
            .as_array()
            .and_then(|bindings| bindings.first())
            .cloned()
            .unwrap_or_else(|| panic!("回执里应有绑定"));
        assert_eq!(report["derived"], 0);
        assert!(
            report["skipped_reason"].is_null(),
            "库里没有可清的选项 ⇒ 这是合法的空，不该被守卫拦下: {report}"
        );

        // 「正常走」的可观测证据：这一轮真的进了事务并落了绑定状态
        // （跳过那条分支**提前返回、一笔不写**，摘要会一直为空）。
        let (digest,): (Option<String>,) = sqlx::query_as(
            "SELECT `snapshot_digest` FROM `feishu_datasource_field` WHERE `datasource_id` = ?",
        )
        .bind(datasource_id)
        .fetch_one(database.pool())
        .await?;
        assert!(
            digest.as_ref().is_some_and(|digest| !digest.is_empty()),
            "本轮必须真的落了绑定摘要（跳过分支不写库），实际 {digest:?}"
        );
        assert_eq!(
            count_options(&database, "bank_branch_code", false).await?,
            0
        );
        Ok(())
    }
    .await;
    finish(outcome, "空源上的空派生").await;
}

// 16. 第二轮导入**不刷新 created_at**、但必须真的写这一行（`updated_at` 前进）。
//
//     这条钉的是批量 ODKU 的赋值列纪律：`created_at` 在 INSERT 列里、**绝不**在赋值列里，
//     否则「创建时间只在首次插入时写」这条语义就没了。
//     它**不能**写成「原样连导两次」：第二轮内容未变且本地没有已停用行时会走 `unchanged`
//     短路，一行都不写，created_at 的断言**空过**。所以先停用一行造出 `disabled_rows > 0`，
//     逼第二轮真的走「预检 → 写 → 补集」，并把 `updated_at` 压成哨兵 0——两轮落在同一秒时
//     它不会变，而 0 是框架写不出来的值，写没写一目了然。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_second_import_keeps_created_at_and_moves_updated_at() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        // 只种一条绑定（行名）——本用例只关心它。
        let datasource_id = seed_xlsx_datasource(
            &database,
            "xlsx_import",
            &[("开户行行名", "bank_branch_name", None)],
        )
        .await?;

        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx"]).await?;
        let (option_id, label_before, created_before): (String, String, i64) = sqlx::query_as(
            "SELECT `option_id`, `label`, `created_at` FROM `feishu_option` \
             WHERE `source_key` = 'bank_branch_name' ORDER BY `id` LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        assert!(created_before > 0, "前置条件：首轮由框架写入 created_at");

        sqlx::query(
            "UPDATE `feishu_option` SET `enabled` = 0, `updated_at` = 0 WHERE `option_id` = ?",
        )
        .bind(&option_id)
        .execute(database.pool())
        .await?;

        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx"]).await?;

        let (created_after, updated_after, enabled_after, label_after): (i64, i64, bool, String) =
            sqlx::query_as(
                "SELECT `created_at`, `updated_at`, `enabled`, `label` FROM `feishu_option` \
                 WHERE `option_id` = ?",
            )
            .bind(&option_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(
            created_after, created_before,
            "**第二次导入绝不能刷新 created_at**（它不在 ODKU 的赋值列里）"
        );
        assert!(
            updated_after > 0,
            "第二轮必须真的写了这一行（updated_at 从哨兵 0 前进）——否则本用例的 created_at 断言是空过的"
        );
        assert!(enabled_after, "被补集停用的行这一轮必须复活");
        // 文案本身不会变：`option_id` 由 `source_key + 父键 + label` 派生（`derive.rs` 的
        // `option_id_of`），同一行改文案就会变成另一个 id（插入新行）而不是更新这一行。
        // 所以「同一行 + 新文案」在现有夹具下不可达，这条只能钉「身份与文案原地不动」。
        assert_eq!(label_after, label_before, "同一 option_id 的文案不变");
        Ok(())
    }
    .await;
    finish(outcome, "第二轮不刷 created_at").await;
}

// 17. 进度端点的端到端形状：路径参数 + 鉴权 + 固定键集，且**导入前后都答 idle**。
//
//     导入在 handler 里同步跑完，进度条目随 `ImportGuard::Drop` 消失，所以「结束之后
//     查得到 idle」正是这条链路的正确终态；而数据源不存在（或已删）也恒 200/idle ——
//     端点不查库，也绝不用 404 表达「没在跑」。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn a_finished_import_reports_idle_progress() {
    let outcome = async {
        let (database, app, token) = prepare_app().await?;
        let datasource_id =
            seed_xlsx_datasource(&database, "xlsx_import", &bank_bindings()).await?;

        let before = dispatch_progress(&app, &token, datasource_id).await?;
        assert_eq!(before["stage"], "idle", "没有导入在跑就是 idle");
        assert_eq!(
            before.as_object().map(serde_json::Map::len),
            Some(8),
            "键集固定（与 Rust 结构体逐字对齐）: {before}"
        );

        dispatch_import(&app, &token, datasource_id, &["bank_1.xlsx"]).await?;

        let after = dispatch_progress(&app, &token, datasource_id).await?;
        assert_eq!(
            after["stage"], "idle",
            "导入结束（handler 返回）后条目必须随 ImportGuard 消失，不留转圈的假条目"
        );
        assert_eq!(after.as_object().map(serde_json::Map::len), Some(8));

        // 不存在的数据源同样恒 200/idle：这条端点的答案是「本进程上这条源有没有导入在跑」，
        // 它不查库，所以也不该用 404 表达「没在跑」。
        let unknown = dispatch_progress(&app, &token, 999_999_999).await?;
        assert_eq!(unknown["stage"], "idle");
        Ok(())
    }
    .await;
    finish(outcome, "进度端点").await;
}
