use crate::app::build_app;
use crate::config::{SchemaMode, Settings};
use crate::transport::http;
use anyhow::Context;
use jsonwebtoken::Algorithm;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use yang_base::database::DatabaseInitializer;
use yang_base::token::TokenManager;
use yang_base::tools::{Tools, ToolsBuilder};
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
            .build()
            .context("构建应用 Tools 失败")?,
    );

    run_then_cleanup(
        run_after_tools_created(&settings, initializer_mysql, Arc::clone(&tools)),
        tools.close(),
    )
    .await
}

/// 运行 Tools 构建后的完整启动与服务阶段。
///
/// 该函数整体位于 [`run_then_cleanup`] 的 operation 边界内，因此应用构建、schema、
/// 地址解析/绑定、服务运行失败以及正常退出都会进入同一个关闭出口。
async fn run_after_tools_created(
    settings: &Settings,
    initializer_mysql: Database,
    tools: Arc<Tools>,
) -> anyhow::Result<()> {
    let application = build_app(Arc::clone(&tools), Arc::new(settings.security.clone()))
        .context("构建应用模块失败")?;

    let initializer = DatabaseInitializer::new(initializer_mysql, false);
    let schema: Vec<_> = application.runtime.table_definitions().iter().collect();
    match settings.schema.mode {
        SchemaMode::Apply => {
            let report = initializer
                .sync_table_definitions(&schema)
                .await
                .context("启动期同步数据库 schema 失败")?;
            tracing::info!(
                tables = ?report.tables,
                changes = report.changes.len(),
                "数据库 schema 同步完成"
            );
        }
        SchemaMode::Validate => {
            let report = initializer
                .plan_table_definitions(&schema)
                .await
                .context("启动期校验数据库 schema 失败")?;
            if !report.is_noop() {
                anyhow::bail!(
                    "数据库 schema 未对齐，存在 {} 项待应用变更: {:?}",
                    report.changes.len(),
                    report.changes
                );
            }
            tracing::info!(tables = ?report.tables, "数据库 schema 校验通过");
        }
        SchemaMode::Off => {
            tracing::warn!("已按配置跳过数据库 schema 同步与校验");
        }
    }
    drop(initializer);

    let bind = settings.bind_addr()?;
    let runtime = Arc::new(application.runtime);
    http::serve(bind, runtime, &settings.http).await
}

/// 等待 operation 完成后无条件执行且仅执行一次 cleanup，并保留原始结果。
async fn run_then_cleanup<T, E>(
    operation: impl Future<Output = Result<T, E>>,
    cleanup: impl Future<Output = ()>,
) -> Result<T, E> {
    let result = operation.await;
    cleanup.await;
    result
}

fn init_tracing(filter: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter).context("logging.filter 无效")?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|error| anyhow::anyhow!("初始化 tracing 失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yang_base::tools::ToolsState;

    #[tokio::test]
    async fn cleanup_runs_exactly_once_for_success_and_every_post_tools_failure() {
        for failed_stage in [
            None,
            Some("build"),
            Some("schema"),
            Some("bind"),
            Some("serve"),
        ] {
            let cleanup_calls = AtomicUsize::new(0);
            let operation = async move {
                match failed_stage {
                    Some(stage) => Err(anyhow::anyhow!("{stage} failed")),
                    None => Ok("completed"),
                }
            };

            let result = run_then_cleanup(operation, async {
                cleanup_calls.fetch_add(1, Ordering::SeqCst);
            })
            .await;

            assert_eq!(
                cleanup_calls.load(Ordering::SeqCst),
                1,
                "退出场景 {failed_stage:?} 必须恰好 cleanup 一次"
            );
            match (failed_stage, result) {
                (Some(stage), Err(error)) => {
                    assert_eq!(error.to_string(), format!("{stage} failed"));
                }
                (None, Ok(value)) => assert_eq!(value, "completed"),
                (stage, unexpected) => {
                    panic!("退出场景 {stage:?} 返回意外结果: {unexpected:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn cleanup_boundary_reaches_tools_close() {
        let tools = match ToolsBuilder::new().build() {
            Ok(tools) => tools,
            Err(error) => panic!("测试 Tools 应构建成功: {error}"),
        };
        let result: anyhow::Result<()> = run_then_cleanup(
            async { Err(anyhow::anyhow!("schema failed")) },
            tools.close(),
        )
        .await;

        match result {
            Err(error) => assert_eq!(error.to_string(), "schema failed"),
            Ok(()) => panic!("业务失败必须保留"),
        }
        assert_eq!(tools.state(), ToolsState::Closed);
    }
}
