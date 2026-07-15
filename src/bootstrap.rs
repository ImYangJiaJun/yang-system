use crate::app::build_app_router;
use crate::config::Settings;
use crate::transport::http;
use anyhow::Context;
use jsonwebtoken::Algorithm;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use yang_base::action::GlobalTools;
use yang_base::database::{DatabaseInitializer, GlobalRedis};
use yang_base::token::TokenManager;
use yang_db::Database;

pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    let settings = Settings::load(config_path)?;
    init_tracing(&settings.logging.filter)?;
    tracing::info!(app = %settings.app.name, config = %config_path.display(), "开始启动系统");

    let database =
        Database::connect_with_config(&settings.database.url, settings.database_config())
            .await
            .context("连接 MySQL 失败")?;
    let pool = Arc::new(database.pool().clone());

    GlobalRedis::init(&settings.redis.url, settings.redis_config())
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
    let tools = Arc::new(GlobalTools::new(token_manager));
    let app_router = Arc::new(
        build_app_router(Arc::clone(&pool), Arc::new(settings.security.clone()))
            .context("构建应用模块失败")?,
    );

    let initializer = DatabaseInitializer::new(database, false);
    let report = initializer
        .sync_app_schema(app_router.as_ref())
        .await
        .context("启动期同步数据库 schema 失败")?;
    tracing::info!(
        tables = ?report.tables,
        changes = report.changes.len(),
        "数据库 schema 同步完成"
    );
    drop(initializer);

    let bind = settings.bind_addr()?;
    let result = http::serve(
        bind,
        app_router,
        tools,
        Arc::clone(&pool),
        settings.http.max_body_bytes,
    )
    .await;

    GlobalRedis::close().await;
    pool.close().await;
    result
}

fn init_tracing(filter: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter).context("logging.filter 无效")?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|error| anyhow::anyhow!("初始化 tracing 失败: {error}"))
}
