//! 飞书审批「关联外部选项」接口的端到端集成测试。
//!
//! # 覆盖什么
//!
//! 真打取选项端点这一条链路：**装配完整的应用**（`build_app`）→ 经 Registry 派发
//! `feishu.option.approval_options` → 断言飞书契约里那层**字面严格**的
//! `{code,msg,data}` 响应体。设计文档 §10 与实施计划 Task 11 把这批用例列为必做，
//! 而 `tests/feishu_options_integration.rs` 只做 Schema 级验证（该文件头部已声明
//! 「不覆盖 HTTP 层」）——分页与模糊搜索因此长期没有自动化执行过。
//!
//! 之所以必须在这一层而不是单测里测：该端点的两处致命缺陷都发生在**查询构造阶段**
//! （`TableQuery::page` 的页大小上限、keyset 条件对 `filterable` 位的校验），
//! 只有真的把 `FeishuContext` 与真实表定义接起来才会触发。
//!
//! # 关于传输层
//!
//! 派发边界把 [`ResponseBody::Raw`] 收成 [`ResponseAttachment::Raw`]，HTTP 层再把它
//! 渲染成**裸 JSON**（不套框架 `{code,message,data}` 包络）。本文件断言的是那个
//! 附件里的 `body` 文本与 `content_type`；裸渲染本身由 yang-base 的传输层测试守住。
//!
//! # 运行方式
//!
//! ```text
//! YANG_SYSTEM_TEST_DATABASE_URL=mysql://root:yang-local@127.0.0.1:3306/yang_system_test \
//! YANG_SYSTEM_TEST_REDIS_URL=redis://127.0.0.1:6379/15 \
//! cargo test --test feishu_approval_options_integration -- --ignored --test-threads=1
//! ```
//!
//! 本文件**读取** `YANG_SYSTEM_TEST_` 开头的环境变量，`scripts/run_ci.py` 的反向发现
//! 因此会要求把它登记进 `INTEGRATION`——漏登记 `--self-test` 会失败。（判据是文件
//! **内容**里出现该前缀，不是文件名。）

use anyhow::{ensure, Context};
use base64::Engine;
use jsonwebtoken::Algorithm;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use yang_base::action::{ApiResponse, Request, RequestMeta, ResponseAttachment, StepUpManager};
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
///
/// `feishu_datasource_field`（字段绑定）是表级改造新增的：`source_key` 与凭据都从
/// 表级行搬到了这一层（设计 §5），出站按 `source_key` 路由的正是这一层。
const DATASOURCE_TABLE: &str = "feishu_datasource";
const FIELD_TABLE: &str = "feishu_datasource_field";
const OPTION_TABLE: &str = "feishu_option";

/// 数据源 Token 明文。服务端只存它的 SHA-256 摘要，明文只出现在这里。
const DATASOURCE_TOKEN: &str = "integration-datasource-token";
/// 第二个数据源的 Token。`token_hash` 有唯一索引，两条绑定不能共用一份凭据。
const SECOND_DATASOURCE_TOKEN: &str = "integration-second-token";
/// 级联用例里父数据源的 Token，同理必须与子数据源的不同。
const PARENT_DATASOURCE_TOKEN: &str = "integration-parent-token";
/// 错误 Token：用于验证来源校验确实拒绝。
const WRONG_DATASOURCE_TOKEN: &str = "integration-wrong-token";
/// 多维表格写入接口的管理 Token。
const MANAGEMENT_TOKEN: &str = "integration-management-token";
/// 单页上限，与 `approval_options.rs` 的 `PAGE_SIZE` 对齐。
const PAGE_SIZE: usize = 100;

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
    RedisClient::connect_with_config(&url, RedisConfig::default())
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
        DatabaseConfig::default(),
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

/// 启用飞书集成；`encryption_key` 为 `None` 表示明文返回。
fn feishu_settings(encryption_key: Option<&str>) -> Arc<FeishuSettings> {
    Arc::new(FeishuSettings {
        enabled: true,
        management_api_token: MANAGEMENT_TOKEN.to_string(),
        encryption_key: encryption_key.map(str::to_string),
        // 本测试只覆盖**入站**端点（飞书来取选项 / 多维表格来推选项），
        // 因此出站凭证保持未配置：`can_pull()` 为假，出站路径整条不参与。
        app_id: None,
        app_secret: None,
        pull_interval_seconds: 900,
        // 告警收件人留空 = 不告警（默认值）；本测试不出站，告警路径不参与。
        alert_recipients: Vec::new(),
        alert_failure_threshold: 3,
    })
}

fn token_manager() -> TokenManager {
    TokenManager::new_symmetric(
        "feishu-integration-token-secret-32-byte",
        Algorithm::HS256,
        "feishu-integration".to_string(),
        "feishu-integration-api".to_string(),
        300,
        3600,
    )
    .unwrap_or_else(|error| panic!("测试 TokenManager 应构建成功: {error}"))
}

fn step_up_manager() -> Arc<StepUpManager> {
    Arc::new(
        StepUpManager::new(
            "feishu-integration-step-up-secret-32byte",
            "feishu-integration-step-up",
            "feishu-sensitive-actions",
        )
        .unwrap_or_else(|error| panic!("集成测试 Step-up manager 应有效: {error}")),
    )
}

/// 装配一个启用飞书集成的完整应用。
async fn build_feishu_app(
    database: &Database,
    redis: &RedisClient,
    encryption_key: Option<&str>,
) -> anyhow::Result<Arc<BuiltApp>> {
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(Database::from_pool(
                database.pool().clone(),
                DatabaseConfig::default(),
            )?)
            .cache(redis.clone())
            .token(token_manager())
            .extension(AuthorizationVersionCache::new(
                redis.clone(),
                "feishu-integration".to_string(),
            )?)
            .extension(step_up_manager())
            .config(feishu_settings(encryption_key))
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
        RequestMeta::new().with_peer_addr(SocketAddr::from(([127, 0, 0, 1], 41_001))),
    );
    let handle = action_handle(app, module, action)
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    app.dispatch_context(handle, context).await
}

/// 调取选项端点并取出飞书信封的 `data.result`。
///
/// 断言 HTTP 层契约（裸 JSON、`application/json`）后解析响应体；`code != 0` 时
/// 直接把整个信封返回，交由调用方断言。
async fn call_approval_options(
    app: &BuiltApp,
    source_key: &str,
    body: Value,
) -> anyhow::Result<Value> {
    let response = dispatch(
        app,
        "feishu.option",
        "approval_options",
        body,
        &[("source_key", source_key)],
        &[],
    )
    .await
    .context("取选项端点返回了 Err——飞书会按「接口报错」处理，必须恒为 200 + 信封")?;

    let Some(ResponseAttachment::Raw { body, content_type }) = response.attachment else {
        anyhow::bail!("取选项端点必须返回裸响应体（ResponseBody::raw），实际: {response:?}");
    };
    ensure!(
        content_type == "application/json",
        "Content-Type 必须是 application/json，实际 {content_type}"
    );
    // 裸响应体不得被框架包络二次包裹
    ensure!(
        !body.contains("\"message\""),
        "裸响应体不得出现框架的 message 键: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).context("响应体应是合法 JSON")?;
    ensure!(
        envelope.get("msg").is_some(),
        "顶层必须是 msg 键（不是 message）: {body}"
    );
    Ok(envelope)
}

// ---------------------------------------------------------------- 数据播种

/// 写入一条**表级**数据源行，返回它的 `id`。
///
/// 表级行只承载「表」这一层：`title` / `status` / 坐标。凭据与 `source_key` 都在
/// 下面的绑定行上（设计 §5）。
async fn seed_datasource_row(
    database: &Database,
    title: &str,
    status: &str,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO `feishu_datasource` (`title`, `status`, `ingest_mode`, `created_at`, `updated_at`) \
         VALUES (?, ?, 'push', NOW(), NOW())",
    )
    .bind(title)
    .bind(status)
    .execute(database.pool())
    .await
    .with_context(|| format!("写入表级数据源 {title} 失败"))?;
    Ok(result.last_insert_id() as i64)
}

/// 写入一条**字段绑定**行：`source_key` 与凭据都住在这里（设计 §5）。
///
/// `parent_field_id` 指向**同一张表**里的另一条绑定——级联由此表达，不再有
/// `linkage_mapping` JSON 列（设计 §8）。
#[allow(clippy::too_many_arguments)] // 绑定行本就这么多列，为测试收个结构体反而更绕
async fn seed_binding(
    database: &Database,
    datasource_id: i64,
    field_id: &str,
    source_key: &str,
    token: &str,
    encrypt_enabled: bool,
    default_locale: &str,
    parent_field_id: Option<&str>,
    enabled: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO `feishu_datasource_field` \
         (`datasource_id`, `field_id`, `source_key`, `token_hash`, `encrypt_enabled`, \
          `default_locale`, `parent_field_id`, `enabled`, `created_at`, `updated_at`) \
         VALUES (?, ?, ?, SHA2(?, 256), ?, ?, ?, ?, NOW(), NOW())",
    )
    .bind(datasource_id)
    .bind(field_id)
    .bind(source_key)
    .bind(token)
    .bind(encrypt_enabled)
    .bind(default_locale)
    .bind(parent_field_id)
    .bind(enabled)
    .execute(database.pool())
    .await
    .with_context(|| format!("写入字段绑定 {source_key} 失败"))?;
    Ok(())
}

/// 播种一个**可出数**的数据源：一条表级行 + 它的一条启用绑定。
///
/// 绝大多数用例只需要「按 `source_key` 能取到选项」这一个前提，两条行的细节无关紧要，
/// 故收成一个入口。`status` 为 `active` / `disabled`。
async fn seed_datasource(
    database: &Database,
    source_key: &str,
    token: &str,
    status: &str,
    default_locale: &str,
    encrypt_enabled: bool,
) -> anyhow::Result<()> {
    let datasource_id =
        seed_datasource_row(database, &format!("测试数据源 {source_key}"), status).await?;
    seed_binding(
        database,
        datasource_id,
        "fld_main",
        source_key,
        token,
        encrypt_enabled,
        default_locale,
        None,
        true,
    )
    .await
}

/// 播种一条选项。
async fn seed_option(
    database: &Database,
    source_key: &str,
    option_id: &str,
    label: &str,
    sort_order: i64,
    enabled: bool,
    i18n: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO `feishu_option` \
         (`option_id`, `source_key`, `label`, `i18n`, `sort_order`, `is_default`, `enabled`, `created_at`, `updated_at`) \
         VALUES (?, ?, ?, ?, ?, 0, ?, NOW(), NOW())",
    )
    .bind(option_id)
    .bind(source_key)
    .bind(label)
    .bind(i18n)
    .bind(sort_order)
    .bind(enabled)
    .execute(database.pool())
    .await
    .with_context(|| format!("写入选项 {option_id} 失败"))?;
    Ok(())
}

/// 播种一条**带父键**的选项（级联用例用）。
///
/// `parent_key` 存的是**父数据源**里那条父选项的 `option_id`（裸值，不带 `@i18n@`），
/// 与 `derive.rs` 的落库口径一致。
async fn seed_option_with_parent(
    database: &Database,
    source_key: &str,
    option_id: &str,
    label: &str,
    sort_order: i64,
    parent_key: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO `feishu_option` \
         (`option_id`, `source_key`, `label`, `parent_key`, `sort_order`, `is_default`, `enabled`, `created_at`, `updated_at`) \
         VALUES (?, ?, ?, ?, ?, 0, 1, NOW(), NOW())",
    )
    .bind(option_id)
    .bind(source_key)
    .bind(label)
    .bind(parent_key)
    .bind(sort_order)
    .execute(database.pool())
    .await
    .with_context(|| format!("写入选项 {option_id} 失败"))?;
    Ok(())
}

/// 批量播种 `count` 条选项，`sort_order` 从 0 递增，文案为 `选项{n}`。
async fn seed_options(database: &Database, source_key: &str, count: usize) -> anyhow::Result<()> {
    for index in 0..count {
        seed_option(
            database,
            source_key,
            &format!("opt_{index:03}"),
            &format!("选项{index:03}"),
            index as i64,
            true,
            None,
        )
        .await?;
    }
    Ok(())
}

/// 批量播种 `count` 条选项，**每 3 条共用一个 `sort_order`**。
///
/// 用于覆盖 keyset 的并列分支：`sort_order` 不再唯一时，跨页只能靠
/// `sort_order = ? AND option_id > ?` 推进。
async fn seed_options_with_tied_sort_order(
    database: &Database,
    source_key: &str,
    count: usize,
) -> anyhow::Result<()> {
    for index in 0..count {
        seed_option(
            database,
            source_key,
            &format!("opt_{index:03}"),
            &format!("选项{index:03}"),
            (index / 3) as i64,
            true,
            None,
        )
        .await?;
    }
    Ok(())
}

/// 读取某个选项当前归属的数据源；不存在返回 `None`。
async fn option_owner(database: &Database, option_id: &str) -> anyhow::Result<Option<String>> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT `source_key` FROM `feishu_option` WHERE `option_id` = ?")
            .bind(option_id)
            .fetch_optional(database.pool())
            .await
            .context("读取选项归属失败")?;
    Ok(owner)
}

/// 取响应里的 `data.result`；`code != 0` 时报错并带上 msg。
fn result_of(envelope: &Value) -> anyhow::Result<&Value> {
    ensure!(
        envelope["code"] == json!(0),
        "期望成功信封，实际: {envelope}"
    );
    envelope
        .get("data")
        .and_then(|data| data.get("result"))
        .context("成功信封必须带 data.result")
}

/// 取 `data.result.options` 里的全部选项 id。
fn option_ids(result: &Value) -> anyhow::Result<Vec<String>> {
    let options = result["options"]
        .as_array()
        .context("result.options 必须是数组")?;
    options
        .iter()
        .map(|option| {
            option["id"]
                .as_str()
                .map(str::to_string)
                .context("option.id 必须是字符串")
        })
        .collect()
}

// ---------------------------------------------------------------- 用例

/// 取选项端点的**主成功路径**：正确 Token 必须拿到第一页选项。
///
/// 这条曾经必失败：`approval_options.rs` 用 `page(1, PAGE_SIZE + 1)` 多取一行判定
/// `hasMore`，而框架 `TableQuery::page` 的页大小硬上限是 100（超出直接 `Err`，
/// 不 clamp）。该 `Err` 被收口成 `code=50001` 的业务失败信封——凡是 token 正确的
/// 请求，一条选项都拿不到。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_returns_the_first_page_with_a_next_page_token() {
    let outcome = async {
        let database = connect_database().await?;
        drop_feishu_tables(&database).await?;
        sync_feishu_schema(database).await?;
        let database = connect_database().await?;
        let redis = connect_redis().await?;

        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        // 比一页多 5 条：确保 hasMore 为真且下一页恰好 5 条
        seed_options(&database, "demo", PAGE_SIZE + 5).await?;

        let app = build_feishu_app(&database, &redis, None).await?;
        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        let result = result_of(&envelope)?;

        let ids = option_ids(result)?;
        ensure!(
            ids.len() == PAGE_SIZE,
            "第一页必须满 {PAGE_SIZE} 条，实际 {}",
            ids.len()
        );
        ensure!(
            result["hasMore"] == json!(true),
            "还有下一页时 hasMore 必须为 true: {result}"
        );
        ensure!(
            result["nextPageToken"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
            "hasMore 为 true 时必须同时返回 nextPageToken: {result}"
        );
        // i18nResources 是文档唯一明示「必须返回」的字段，返回空会让控件显示为空
        let resources = result["i18nResources"]
            .as_array()
            .context("i18nResources 必须是数组")?;
        ensure!(!resources.is_empty(), "i18nResources 不得为空");
        ensure!(
            resources[0]["locale"] == json!("zh_cn") && resources[0]["isDefault"] == json!(true),
            "至少回一条默认语言并标 isDefault: {resources:?}"
        );
        ensure!(
            result["options"][0]["value"] == json!("@i18n@opt_000"),
            "value 必须是 @i18n@<option_id> 占位符: {result}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("取选项主路径集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 建表 → 重建连接 → 装配启用飞书集成的应用；调用方随后自行播种数据。
///
/// `sync_with_database` 会关掉连接池，所以这里必须在同步之后重新建连——否则后面
/// 拿到的 `pool` 已经是关闭的。
async fn prepare_app(encryption_key: Option<&str>) -> anyhow::Result<(Database, Arc<BuiltApp>)> {
    let database = connect_database().await?;
    drop_feishu_tables(&database).await?;
    sync_feishu_schema(database).await?;
    let database = connect_database().await?;
    let redis = connect_redis().await?;
    let app = build_feishu_app(&database, &redis, encryption_key).await?;
    Ok((database, app))
}

/// 按飞书文档的 Go 参考实现解回 `data.result`：AES-256-CBC，
/// key = SHA-256(配置原文)，IV 是密文前 16 字节，填充为 PKCS#7。
///
/// 生产路径只加密不解密，所以这里在测试侧独立复刻一遍——它同时是「我们的密文能被
/// 第三方参考实现解开」这一互操作性主张的可执行证据。
fn decrypt_result(cipher_base64: &str, key_raw: &str) -> anyhow::Result<Value> {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    use sha2::{Digest, Sha256};

    let key = Sha256::digest(key_raw.as_bytes());
    let raw = base64::engine::general_purpose::STANDARD
        .decode(cipher_base64)
        .context("data.result 应是标准 base64")?;
    ensure!(raw.len() > 16 && (raw.len() - 16) % 16 == 0, "密文长度非法");
    let (iv, body) = raw.split_at(16);

    let mut buffer = body.to_vec();
    let plain = cbc::Decryptor::<aes::Aes256>::new(key.as_slice().into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|error| anyhow::anyhow!("AES-CBC 解密失败: {error}"))?;
    serde_json::from_slice(plain).context("解密结果应是合法 JSON")
}

/// 错误 Token 必须被拒，且不得泄漏任何选项。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_rejects_a_wrong_token() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        seed_options(&database, "demo", 3).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": WRONG_DATASOURCE_TOKEN })).await?;
        // 断言**具体**码而不是「非 0」：后者无法把「按预期拒绝」与「端点整体坏成
        // 50001」区分开，而后者正是本文件存在的原因。
        ensure!(
            envelope["code"] == json!(40102),
            "错误 Token 必须给出 40102（token 校验失败）: {envelope}"
        );
        ensure!(
            envelope["data"].is_null(),
            "失败信封不得携带 data: {envelope}"
        );
        ensure!(
            !envelope.to_string().contains("opt_000"),
            "失败信封不得泄漏任何选项: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("错误 Token 集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 已停用的数据源即使 Token 正确也必须被拒。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_rejects_a_disabled_datasource() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "disabled",
            "zh_cn",
            false,
        )
        .await?;
        seed_options(&database, "demo", 3).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        ensure!(
            envelope["code"] == json!(40301),
            "停用数据源必须给出 40301（数据源已停用）: {envelope}"
        );
        ensure!(
            !envelope.to_string().contains("opt_000"),
            "停用数据源的选项不得被取出: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("停用数据源集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 被停用的选项不得出现在结果里——历史审批单仍引用它的 id，所以是禁用而非删除。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_hides_disabled_options() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        seed_option(&database, "demo", "opt_live", "在用的", 0, true, None).await?;
        seed_option(&database, "demo", "opt_dead", "停用的", 1, false, None).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        let result = result_of(&envelope)?;
        let ids = option_ids(result)?;
        ensure!(
            ids == vec!["opt_live".to_string()],
            "只应回启用的选项，实际 {ids:?}"
        );
        ensure!(
            !result.to_string().contains("opt_dead"),
            "停用选项的 id 与文案都不得出现: {result}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("停用选项集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 非法 `page_token` 必须 fail-closed（非 0 code），不得静默退回第一页。
///
/// 静默退回会让翻页重复吐第一页，在「多选」控件上表现为选项重复——比报错更难排查。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_rejects_a_malformed_page_token() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        seed_options(&database, "demo", 3).await?;

        let envelope = call_approval_options(
            &app,
            "demo",
            json!({ "token": DATASOURCE_TOKEN, "page_token": "not-base64!!" }),
        )
        .await?;
        ensure!(
            envelope["code"] == json!(40002),
            "非法游标必须给出 40002（分页标记非法）: {envelope}"
        );
        ensure!(
            envelope["data"].is_null(),
            "非法游标不得回退成第一页的数据: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("非法游标集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// `query` 关键词按「包含」匹配 `label` / `option_id`；未命中时必须回空 `options`
/// 但仍带非空的 `i18nResources`（文档唯一明示的必传字段）。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_matches_the_query_keyword() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        // 文案形如 选项000…选项104，因此「选项00」恰好命中 opt_000..opt_009
        seed_options(&database, "demo", PAGE_SIZE + 5).await?;

        let hit = call_approval_options(
            &app,
            "demo",
            json!({ "token": DATASOURCE_TOKEN, "query": "选项00" }),
        )
        .await?;
        let hit_result = result_of(&hit)?;
        let ids = option_ids(hit_result)?;
        ensure!(
            ids.len() == 10,
            "「选项00」应命中 10 条，实际 {}",
            ids.len()
        );
        ensure!(
            ids.iter().all(|id| id.as_str() < "opt_010"),
            "命中的必须是 opt_000..opt_009，实际 {ids:?}"
        );
        ensure!(
            hit_result["hasMore"] == json!(false),
            "命中集不足一页时 hasMore 必须为 false"
        );

        let miss = call_approval_options(
            &app,
            "demo",
            json!({ "token": DATASOURCE_TOKEN, "query": "不存在的关键词" }),
        )
        .await?;
        let miss_result = result_of(&miss)?;
        ensure!(
            option_ids(miss_result)?.is_empty(),
            "未命中应回空 options: {miss_result}"
        );
        ensure!(
            !miss_result["i18nResources"]
                .as_array()
                .context("i18nResources 必须是数组")?
                .is_empty(),
            "即使未命中，i18nResources 也必须非空——返回空会让控件显示为空"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("关键词检索集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 数据源开启加密后 `data.result` 必须是 base64 字符串，且能被文档给出的参考实现解开。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_encrypts_the_result_when_the_datasource_requires_it() {
    const ENCRYPTION_KEY: &str = "integration-encryption-key";

    let outcome = async {
        let (database, app) = prepare_app(Some(ENCRYPTION_KEY)).await?;
        seed_datasource(&database, "demo", DATASOURCE_TOKEN, "active", "zh_cn", true).await?;
        seed_option(&database, "demo", "opt_a", "甲", 0, true, None).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        ensure!(envelope["code"] == json!(0), "加密路径也应成功: {envelope}");
        let cipher = envelope["data"]["result"]
            .as_str()
            .context("配置 Key 后 data.result 必须是字符串（不是对象）")?;

        // 明文里不得出现可读内容
        ensure!(
            !cipher.contains("opt_a") && !envelope.to_string().contains("甲"),
            "密文不得泄漏明文"
        );

        let plain = decrypt_result(cipher, ENCRYPTION_KEY)?;
        ensure!(
            plain["options"][0]["id"] == json!("opt_a"),
            "解密后应还原明文 result，实际 {plain}"
        );
        ensure!(
            plain["options"][0]["value"] == json!("@i18n@opt_a"),
            "解密后 value 仍是 @i18n@ 占位符: {plain}"
        );
        ensure!(
            plain["i18nResources"]
                .as_array()
                .is_some_and(|resources| !resources.is_empty()),
            "解密后 i18nResources 仍必须非空: {plain}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("加密路径集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 同一批 upsert 调两次，行数不变——多维表格自动化重推同一行是常态。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn upsert_is_idempotent_within_the_same_datasource() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");
        let payload = json!({
            "source_key": "demo",
            "options": [
                { "id": "opt_a", "label": "甲" },
                { "id": "opt_b", "label": "乙" },
            ],
        });

        for _ in 0..2 {
            let response = dispatch(
                &app,
                "feishu.option",
                "upsert_options",
                payload.clone(),
                &[],
                &[("authorization", bearer.as_str())],
            )
            .await
            .context("upsert 不应返回 Err")?;
            ensure!(response.code == 0, "upsert 必须成功: {response:?}");
        }

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `feishu_option`")
            .fetch_one(database.pool())
            .await
            .context("统计选项行数失败")?;
        ensure!(rows == 2, "重推两次后仍应是 2 行，实际 {rows}");

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("upsert 幂等集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 受保护的控制台 Action 对匿名调用返回错误。
///
/// # 这条用例守不住什么（别高估它）
///
/// 它**不是** `authenticate_public_actions()` 去留的判据：`list_options` 未声明
/// `.public()`，而中间件链耗尽后会落到链尾的 `authorize()`，那里对非 public Action
/// 恒要求已认证身份。所以「模块根本没挂 TokenAuthMiddleware」「挂着带
/// `authenticate_public_actions()` 的中间件」「本次修复态」三种状态下它都通过。
///
/// 真正钉住本次改动的是另外两条：`write_endpoints_accept_the_management_token_...`
/// （带管理 Token 的写入不得被短路）与
/// `approval_options_ignores_an_unrelated_authorization_header`（public 端点不得被无关
/// 的 `Authorization` 头打成 401）。保留本用例只是因为「非 public Action 拒绝匿名」
/// 本身仍是一条值得钉住的不变量。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn protected_console_actions_still_require_authentication() {
    let outcome = async {
        let (_database, app) = prepare_app(None).await?;

        let anonymous = dispatch(
            &app,
            "feishu.option",
            "list_options",
            json!({ "count_total": true }),
            &[],
            &[],
        )
        .await;
        ensure!(
            anonymous.is_err(),
            "无身份调用受保护 Action 必须被拒，实际: {anonymous:?}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("受保护 Action 鉴权集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 一个数据源不能把另一个数据源已有的选项「夺」过来。
///
/// 这条曾经必失败：`upsert_options` 的 UPDATE 只用 `where_eq("option_id")` 定位，
/// 而写入的记录里带着**本次请求的** `source_key`——于是 B 推一个已在 A 名下的
/// `option_id`，会把那行的 `source_key` 直接改写成 B，A 的取选项接口就少了一条
/// （`options` 与 `texts` 同时变），而且两边都看不到任何异常信号。
///
/// 同一数据源内的更新必须照旧可用——这条一并钉住，避免把「限定 source_key」
/// 修成「任何更新都不生效」。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn upsert_cannot_move_an_option_across_datasources() {
    let outcome = async {
        let database = connect_database().await?;
        drop_feishu_tables(&database).await?;
        sync_feishu_schema(database).await?;
        let database = connect_database().await?;
        let redis = connect_redis().await?;

        seed_datasource(
            &database,
            "alpha",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        // beta 必须用**另一个** Token：`token_hash` 有唯一索引，两条绑定共用一份
        // 凭据会撞在写入那一步（H3/Ruling 40）。
        seed_datasource(
            &database,
            "beta",
            SECOND_DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        seed_option(&database, "alpha", "shared", "甲方的选项", 0, true, None).await?;

        let app = build_feishu_app(&database, &redis, None).await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");

        // beta 试图用同一个 option_id 写自己的选项
        let stolen = dispatch(
            &app,
            "feishu.option",
            "upsert_options",
            json!({
                "source_key": "beta",
                "options": [{ "id": "shared", "label": "乙方的选项" }],
            }),
            &[],
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("跨数据源写入不应返回 Err，应给出可归因的业务失败")?;
        ensure!(
            stolen.code != 0,
            "跨数据源夺取选项必须被拒，实际: {stolen:?}"
        );
        ensure!(
            option_owner(&database, "shared").await?.as_deref() == Some("alpha"),
            "选项归属不得被改写"
        );

        // 同一数据源内的更新必须照旧生效
        let updated = dispatch(
            &app,
            "feishu.option",
            "upsert_options",
            json!({
                "source_key": "alpha",
                "options": [{ "id": "shared", "label": "甲方改名后的选项" }],
            }),
            &[],
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("同数据源更新不应返回 Err")?;
        ensure!(updated.code == 0, "同数据源更新必须成功: {updated:?}");
        let label: Option<String> =
            sqlx::query_scalar("SELECT `label` FROM `feishu_option` WHERE `option_id` = 'shared'")
                .fetch_optional(database.pool())
                .await
                .context("读取选项文案失败")?;
        ensure!(
            label.as_deref() == Some("甲方改名后的选项"),
            "同数据源更新必须真的改写文案，实际 {label:?}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("跨数据源写入集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 排序键并列时，跨页必须靠 `(sort_order, option_id)` 的后半段推进。
///
/// 游标是复合键 `(sort_order, option_id)`，keyset 条件为
/// `sort_order > ? OR (sort_order = ? AND option_id > ?)`。上一页正好在某一组
/// `sort_order` **中间**截断时，下一页的第一批行只能由第二个分支取到；那一支写反或
/// 漏掉就会漏行。只播种互不相同的 `sort_order` 是**取不到**这一支的——本用例是它唯一
/// 的执行证据。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_advances_across_rows_that_share_a_sort_order() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        // 150 条、每 3 条共用一个 sort_order：第 100 行（opt_099）的 sort_order 是 33，
        // 同组还有 opt_100 / opt_101 —— 它们只能靠并列分支取到。
        let seeded = 150;
        seed_options_with_tied_sort_order(&database, "demo", seeded).await?;

        let first =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        let first_result = result_of(&first)?;
        let mut seen = option_ids(first_result)?;
        ensure!(seen.len() == PAGE_SIZE, "第一页应满 {PAGE_SIZE} 条");
        let cursor = first_result["nextPageToken"]
            .as_str()
            .context("第一页必须给出 nextPageToken")?
            .to_string();

        let second = call_approval_options(
            &app,
            "demo",
            json!({ "token": DATASOURCE_TOKEN, "page_token": cursor }),
        )
        .await?;
        let second_result = result_of(&second)?;
        let second_ids = option_ids(second_result)?;
        ensure!(
            second_ids.contains(&"opt_100".to_string())
                && second_ids.contains(&"opt_101".to_string()),
            "并列分支必须取到同 sort_order 的后续行，第二页实际 {second_ids:?}"
        );

        seen.extend(second_ids);
        let unique: std::collections::BTreeSet<&String> = seen.iter().collect();
        ensure!(
            unique.len() == seen.len(),
            "并列排序键下翻页不得重复：{} 条中有 {} 条唯一",
            seen.len(),
            unique.len()
        );
        ensure!(
            seen.len() == seeded && (0..seeded).all(|i| seen.contains(&format!("opt_{i:03}"))),
            "并列排序键下翻页不得漏行：应覆盖 {seeded} 条，实际 {}",
            seen.len()
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("并列排序键翻页集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 数据源开了加密但服务端没配密钥：必须给出可归因的业务失败信封（`50002`），
/// 而不是静默按明文返回、也不是一个裸 500。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_reports_when_encryption_is_enabled_but_unconfigured() {
    let outcome = async {
        // 注意密钥为 None，而数据源要求加密
        let (database, app) = prepare_app(None).await?;
        seed_datasource(&database, "demo", DATASOURCE_TOKEN, "active", "zh_cn", true).await?;
        seed_option(&database, "demo", "opt_a", "甲", 0, true, None).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        ensure!(
            envelope["code"] == json!(50002),
            "缺密钥必须给出 50002（服务端未配置加密密钥），而不是静默明文返回: {envelope}"
        );
        ensure!(envelope["data"].is_null(), "失败信封不得带数据: {envelope}");
        ensure!(
            !envelope.to_string().contains("甲"),
            "失败时不得泄漏明文选项: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("缺加密密钥集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 数据源不存在时给出 `40401`，且不泄漏任何选项。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_reports_a_missing_datasource() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        seed_options(&database, "demo", 3).await?;

        let envelope = call_approval_options(
            &app,
            "not_a_datasource",
            json!({ "token": DATASOURCE_TOKEN }),
        )
        .await?;
        ensure!(
            envelope["code"] == json!(40401),
            "数据源不存在应给出 40401，实际: {envelope}"
        );
        ensure!(
            !envelope.to_string().contains("opt_000"),
            "不得泄漏其它数据源的选项: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("数据源不存在集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 取选项端点是 public 的：调用方多带一个 `Authorization` 头**不得**把它打成 401。
///
/// 这条钉住的是 `with_authentication` 里 `authenticate_public_actions()` 的去留：
/// 它把 `TokenAuthMiddleware` 的 scope 抬到 `AllActions`，于是飞书端点也会被拿这个头去
/// 验签。飞书按官方契约把凭证放在**请求体**的 `token` 字段，`Authorization` 头不属于
/// 它的契约——一个网关/客户端顺手附上的头不该让取选项整体失败。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_ignores_an_unrelated_authorization_header() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        seed_option(&database, "demo", "opt_a", "甲", 0, true, None).await?;

        // 一个不是本系统签发的 JWT 的 Bearer 值：必须被忽略，而不是让端点短路成 401
        let envelope = dispatch(
            &app,
            "feishu.option",
            "approval_options",
            json!({ "token": DATASOURCE_TOKEN }),
            &[("source_key", "demo")],
            &[("authorization", "Bearer not-a-jwt-from-this-system")],
        )
        .await
        .context("带无关 Authorization 头时端点不应返回 Err（会被飞书当成接口报错）")?;
        let Some(ResponseAttachment::Raw { body, .. }) = envelope.attachment else {
            anyhow::bail!("必须返回裸响应体，实际: {envelope:?}");
        };
        let parsed: Value = serde_json::from_str(&body).context("响应体应是合法 JSON")?;
        ensure!(
            parsed["code"] == json!(0),
            "无关的 Authorization 头不得击穿 public 的取选项端点: {parsed}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("无关 Authorization 头集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 归属判定必须与写入用**同一套比较语义**。
///
/// 表的排序规则是 `utf8mb4_unicode_ci`（大小写不敏感 + PAD SPACE），所以「数据源是否存在」
/// 的检查与 UPDATE 的条件都把 `DEMO` 与 `demo` 视为同一个数据源。若归属预检改用 Rust 的
/// 逐字节比较，就会出现「存在性检查认它是自己、写入条件也认它是自己，唯独预检说它是别人」
/// ——调用方拿着自己数据源的另一种拼写被 40901 拒绝，而错误文案还指向它自己。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn upsert_compares_ownership_with_the_same_collation_as_the_write() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        // 关键：选项**已存在**于 demo 名下，预检才会真的去比对归属
        seed_option(&database, "demo", "opt_a", "原文案", 0, true, None).await?;
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");

        // 同一数据源的另一种拼写：存在性检查与 UPDATE 都按 ci 排序规则认它，预检也必须认
        let response = dispatch(
            &app,
            "feishu.option",
            "upsert_options",
            json!({
                "source_key": "DEMO",
                "options": [{ "id": "opt_a", "label": "改后的文案" }],
            }),
            &[],
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("同数据源的另一种拼写不应返回 Err")?;
        ensure!(
            response.code == 0,
            "存在性检查已经把 DEMO 认作 demo，预检就不该判它是「另一个数据源」: {response:?}"
        );
        let label: Option<String> =
            sqlx::query_scalar("SELECT `label` FROM `feishu_option` WHERE `option_id` = 'opt_a'")
                .fetch_optional(database.pool())
                .await
                .context("读取选项文案失败")?;
        ensure!(
            label.as_deref() == Some("改后的文案"),
            "同数据源的更新必须真的生效，实际 {label:?}"
        );
        // 归属仍是同一个数据源。注意**不能**断言逐字节等于 `demo`：UPDATE 会把记录里的
        // `source_key` 一并写回，于是存储侧留下调用方的拼写（`DEMO`）。在
        // `utf8mb4_unicode_ci` 下两者是同一个键——本表的所有读路径（数据源是否存在、
        // 取选项的 path 过滤、删除数据源时连带停用）都用这条排序规则，所以这是无害的
        // 大小写漂移，不是归属变更。
        ensure!(
            option_owner(&database, "opt_a")
                .await?
                .is_some_and(|owner| owner.eq_ignore_ascii_case("demo")),
            "归属必须仍是同一个数据源"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("归属比较语义集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 写入入口必须按契约接受 `Authorization: Bearer <管理 Token>`，匿名调用必须被拒。
///
/// 这条曾经必失败：`with_authentication` 挂了 `.authenticate_public_actions()`，
/// 把 `TokenAuthMiddleware` 的 scope 从 `ProtectedActions` 抬到 `AllActions`，
/// 于是它排到 `ManagementTokenMiddleware` **前面**，把两者共用的 `authorization`
/// 头当成 Access JWT 去验签并直接短路——管理 Token 永远到不了校验它的那道门。
/// 不带该头则被管理 Token 中间件拒。两条路都不通，写入入口在任何调用方式下不可用。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn write_endpoints_accept_the_management_token_and_reject_anonymous_callers() {
    let outcome = async {
        let database = connect_database().await?;
        drop_feishu_tables(&database).await?;
        sync_feishu_schema(database).await?;
        let database = connect_database().await?;
        let redis = connect_redis().await?;

        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        let app = build_feishu_app(&database, &redis, None).await?;

        let payload = json!({
            "source_key": "demo",
            "options": [{ "id": "opt_a", "label": "甲" }],
        });

        // 不带凭证：必须被管理 Token 中间件以业务失败拒掉（不是 404、也不是放行）
        let anonymous = dispatch(
            &app,
            "feishu.option",
            "upsert_options",
            payload.clone(),
            &[],
            &[],
        )
        .await
        .context("匿名调用写入入口不应返回 Err，应由中间件给出业务失败")?;
        ensure!(
            anonymous.code == 40102,
            "无管理 Token 必须被拒，实际: {anonymous:?}"
        );
        ensure!(
            option_owner(&database, "opt_a").await?.is_none(),
            "被拒的请求不得写入任何数据"
        );

        // 按契约带 Bearer：必须放行
        let bearer = format!("Bearer {MANAGEMENT_TOKEN}");
        let authorized = dispatch(
            &app,
            "feishu.option",
            "upsert_options",
            payload,
            &[],
            &[("authorization", bearer.as_str())],
        )
        .await
        .context("带管理 Token 的写入不应返回 Err——静态 Token 不是 Access JWT，不该被验签短路")?;
        ensure!(
            authorized.code == 0,
            "带管理 Token 必须放行，实际: {authorized:?}"
        );
        ensure!(
            option_owner(&database, "opt_a").await?.as_deref() == Some("demo"),
            "放行的写入必须真的落库"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("写入入口鉴权集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 分页游标必须能推进到下一页，且跨页**不漏不重**。
///
/// 这条曾经必失败，且与上一条的根因**彼此独立**：keyset 的 `where_or` 对
/// `sort_order` 施加了 `Gt` / `Eq` 条件，而 `where_or` 会经 `validate_condition_tree`
/// 逐叶调用 `validate_filter_field`——DSL 的 `filterable` 是 fail-closed，`sort_order`
/// 只声明了 `.sortable(true)`，于是校验期直接 `FieldPermissionDenied`。即使把页大小
/// 修好，**带 `page_token` 的请求从第二页起仍然全败**。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_advances_the_cursor_without_losing_or_repeating_options() {
    let outcome = async {
        let database = connect_database().await?;
        drop_feishu_tables(&database).await?;
        sync_feishu_schema(database).await?;
        let database = connect_database().await?;
        let redis = connect_redis().await?;

        seed_datasource(
            &database,
            "demo",
            DATASOURCE_TOKEN,
            "active",
            "zh_cn",
            false,
        )
        .await?;
        let seeded = PAGE_SIZE + 5;
        seed_options(&database, "demo", seeded).await?;

        let app = build_feishu_app(&database, &redis, None).await?;

        let first =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        let first_result = result_of(&first)?;
        let mut seen = option_ids(first_result)?;
        let cursor = first_result["nextPageToken"]
            .as_str()
            .context("第一页必须给出 nextPageToken")?
            .to_string();

        let second = call_approval_options(
            &app,
            "demo",
            json!({ "token": DATASOURCE_TOKEN, "page_token": cursor }),
        )
        .await?;
        let second_result = result_of(&second)?;
        let second_ids = option_ids(second_result)?;
        ensure!(
            second_ids.len() == 5,
            "第二页应剩 5 条，实际 {}: {second_result}",
            second_ids.len()
        );
        ensure!(
            second_result["hasMore"] == json!(false),
            "最后一页 hasMore 必须为 false: {second_result}"
        );
        ensure!(
            second_result.get("nextPageToken").is_none(),
            "hasMore 为 false 时不得返回 nextPageToken: {second_result}"
        );

        // 跨页不漏不重：并集必须恰好等于播种的全集
        seen.extend(second_ids);
        let unique: std::collections::BTreeSet<&String> = seen.iter().collect();
        ensure!(
            unique.len() == seen.len(),
            "翻页出现重复选项：{} 条中有 {} 条唯一",
            seen.len(),
            unique.len()
        );
        let expected: Vec<String> = (0..seeded).map(|index| format!("opt_{index:03}")).collect();
        ensure!(
            seen.len() == expected.len() && expected.iter().all(|id| seen.contains(id)),
            "翻页并集应覆盖全部 {seeded} 条，实际 {} 条",
            seen.len()
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("翻页集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 级联：读端按绑定行的 `parent_field_id` 找父、用父的 `source_key` 过滤子项。
///
/// 这条是出站回归修复的端到端证据。表级改造删掉了 `linkage_mapping` 列，级联改由
/// 两条绑定的 `parent_field_id` 关联表达；此前读端仍在读那个不存在的列，端点对每个
/// 合法请求恒失败（见文件头与本次修复的 ledger）。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_filters_by_the_parent_binding() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;

        // 级联的父与子必须是**同一张表**（同一个 `datasource_id`）里的两条绑定——
        // `parent_field_id` 指的就是同表另一列（设计 §8）。两条绑定各有自己的
        // `source_key`，所以选项仍分属两个「数据源」。
        let table_id = seed_datasource_row(&database, "币种汇率表", "active").await?;
        seed_binding(
            &database,
            table_id,
            "fld_currency",
            "currency",
            PARENT_DATASOURCE_TOKEN,
            false,
            "zh_cn",
            None,
            true,
        )
        .await?;
        seed_binding(
            &database,
            table_id,
            "fld_rate",
            "rate",
            DATASOURCE_TOKEN,
            false,
            "zh_cn",
            Some("fld_currency"),
            true,
        )
        .await?;

        // 父源下的选项：它们就是飞书回传给我们的父值（`@i18n@<option_id>`）。
        seed_option(
            &database,
            "currency",
            "currency:CNY",
            "人民币",
            0,
            true,
            None,
        )
        .await?;
        seed_option(&database, "currency", "currency:USD", "美元", 1, true, None).await?;
        // 子源下三个选项：两个挂父键，一个挂**不存在**的父键（不该被任何请求带回）。
        seed_option_with_parent(
            &database,
            "rate",
            "rate:cny",
            "人民币汇率",
            0,
            "currency:CNY",
        )
        .await?;
        seed_option_with_parent(&database, "rate", "rate:usd", "美元汇率", 1, "currency:USD")
            .await?;
        seed_option_with_parent(
            &database,
            "rate",
            "rate:orphan",
            "无主的汇率",
            2,
            "currency:GBP",
        )
        .await?;

        // 带父值 → 只回该父下的子项
        let filtered = call_approval_options(
            &app,
            "rate",
            json!({
                "token": DATASOURCE_TOKEN,
                "linkage_params": { "widget17796881173030001": "@i18n@currency:CNY" },
            }),
        )
        .await?;
        let filtered_ids = option_ids(result_of(&filtered)?)?;
        ensure!(
            filtered_ids == vec!["rate:cny".to_string()],
            "只应回父值 currency:CNY 下的子项，实际 {filtered_ids:?}"
        );

        // 不带联动参数 → 回退全量（契约 C3）
        let all = call_approval_options(&app, "rate", json!({ "token": DATASOURCE_TOKEN })).await?;
        let all_ids = option_ids(result_of(&all)?)?;
        ensure!(
            all_ids.len() == 3,
            "不带联动参数必须回退全量，实际 {all_ids:?}"
        );

        // 父值在父源里不存在 → 可归因失败 40004，而不是静默空集
        let unresolved = call_approval_options(
            &app,
            "rate",
            json!({
                "token": DATASOURCE_TOKEN,
                "linkage_params": { "widget1": "@i18n@currency:NOPE" },
            }),
        )
        .await?;
        ensure!(
            unresolved["code"] == json!(40004),
            "父值不存在必须给出 40004（联动值无法解析），实际: {unresolved}"
        );

        // 父指针指向**别的表**里的字段 id → 不算父，按无级联处理（回退全量）
        let other_id = seed_datasource_row(&database, "其它表", "active").await?;
        seed_binding(
            &database,
            other_id,
            "fld_other",
            "other",
            SECOND_DATASOURCE_TOKEN,
            false,
            "zh_cn",
            // 这个 field_id 属于「币种」那张表，不在「其它表」里
            Some("fld_currency"),
            true,
        )
        .await?;
        let cross_table_parent = call_approval_options(
            &app,
            "other",
            json!({
                "token": SECOND_DATASOURCE_TOKEN,
                "linkage_params": { "widget1": "@i18n@currency:CNY" },
            }),
        )
        .await?;
        ensure!(
            cross_table_parent["code"] == json!(0),
            "父不在同一张表时应按无级联处理（回退全量），实际: {cross_table_parent}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("级联过滤集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 绑定本身被停用（`enabled = false`）必须被拒，哪怕表级源仍是 `active`。
///
/// 表级模型下「停用」有两个来源：表级行的 `status` 与绑定行的 `enabled`。旧用例只覆盖
/// 前者；这条钉住后者——取消勾选一条字段就是停用它（设计 §5 的 `enabled`）。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_rejects_a_disabled_binding() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        let datasource_id = seed_datasource_row(&database, "停用绑定的源", "active").await?;
        seed_binding(
            &database,
            datasource_id,
            "fld_main",
            "demo",
            DATASOURCE_TOKEN,
            false,
            "zh_cn",
            None,
            // 表级 active，但这条绑定被停用
            false,
        )
        .await?;
        seed_options(&database, "demo", 3).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        ensure!(
            envelope["code"] == json!(40301),
            "停用的绑定必须给出 40301（数据源已停用），实际: {envelope}"
        );
        ensure!(
            !envelope.to_string().contains("opt_000"),
            "停用绑定的选项不得被取出: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("停用绑定集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 绑定的宿主表级行不存在（孤立绑定）按**停用**处理：绑定还在但表没了，不该继续出数。
///
/// 这条钉住一个刻意选的 fail-closed：`status` 读不出来时不当作 `active`。绑定表没有
/// 外键约束（设计 §5），所以这种行在库层面是可能出现的。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn approval_options_rejects_an_orphaned_binding() {
    let outcome = async {
        let (database, app) = prepare_app(None).await?;
        // 直接指向一个不存在的表级 id
        seed_binding(
            &database,
            9_999_999,
            "fld_main",
            "demo",
            DATASOURCE_TOKEN,
            false,
            "zh_cn",
            None,
            true,
        )
        .await?;
        seed_options(&database, "demo", 3).await?;

        let envelope =
            call_approval_options(&app, "demo", json!({ "token": DATASOURCE_TOKEN })).await?;
        ensure!(
            envelope["code"] == json!(40301),
            "宿主表级行缺失必须按停用处理（40301），实际: {envelope}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_database().await {
        Ok(database) => drop_feishu_tables(&database).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("孤立绑定集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}
