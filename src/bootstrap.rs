use crate::app::build_app;
use crate::config::Settings;
use crate::transport::http;
use anyhow::Context;
use jsonwebtoken::Algorithm;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use yang_base::database::DatabaseInitializer;
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_db::{Database, RedisClient};

pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    let settings = Settings::load(config_path)?;
    init_tracing(&settings.logging.filter)?;
    tracing::info!(app = %settings.app.name, config = %config_path.display(), "开始启动系统");

    let mysql = Database::connect_with_config(&settings.mysql.url, settings.mysql_config())
        .await
        .context("连接 MySQL 失败")?;
    let initializer_mysql = Database::from_pool(mysql.pool().clone(), settings.mysql_config())
        .context("构造 schema 初始化数据库失败")?;
    let cache = RedisClient::connect_with_config(&settings.redis.url, settings.redis_config())
        .await
        .context("连接 Redis 失败")?;

    let token_manager = TokenManager::new_symmetric(
        &settings.token.secret,
        Algorithm::HS256,
        settings.token.issuer.clone(),
        settings.token.audience.clone(),
        settings.token.access_ttl_seconds,
        settings.token.refresh_ttl_seconds,
    );
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(cache)
            .token(token_manager)
            .config(settings.clone())
            .build()
            .context("构建应用 Tools 失败")?,
    );
    let application = build_app(Arc::clone(&tools), Arc::new(settings.security.clone()))
        .context("构建应用模块失败")?;

    let initializer = DatabaseInitializer::new(initializer_mysql, false);
    let schema: Vec<_> = application.runtime.table_definitions().iter().collect();
    let report = initializer
        .sync_table_definitions(&schema)
        .await
        .context("启动期同步数据库 schema 失败")?;
    tracing::info!(
        tables = ?report.tables,
        changes = report.changes.len(),
        "数据库 schema 同步完成"
    );
    drop(initializer);

    let bind = settings.bind_addr()?;
    let runtime = Arc::new(application.runtime);
    let result = http::serve(bind, runtime, settings.http.max_body_bytes).await;

    tools.close().await;
    result
}

fn init_tracing(filter: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter).context("logging.filter 无效")?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|error| anyhow::anyhow!("初始化 tracing 失败: {error}"))
}
