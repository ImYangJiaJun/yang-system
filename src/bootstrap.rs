use crate::addon::account::email_delivery::{
    RegistrationEmailSenderHandle, SmtpRegistrationEmailSender,
};
use crate::app::{build_app, YANG_SYSTEM_METRIC_NAMES};
use crate::authorization::{AuthorizationOutboxWorker, AuthorizationVersionCache};
use crate::config::Settings;
use anyhow::Context;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use yang_base::database::DatabaseInitializer;
use yang_base::tools::{Tools, ToolsBuilder};
use yang_base::transport::axum::{serve_with_shutdown, AxumTransportConfig};
use yang_db::{Database, RedisClient};
use yang_runtime::observability::{LogIdentity, TelemetryRuntime};
use yang_runtime::shutdown::ShutdownBudget;

const TRIGGER_SIGNAL: &str = "signal";
const TRIGGER_OPERATION_EXIT: &str = "operation_exit";
const PHASE_HTTP_DRAIN: &str = "http_drain";
const PHASE_AUTHORIZATION_OUTBOX_WORKER: &str = "authorization_outbox_worker";
const PHASE_TOOLS_CLOSE: &str = "tools_close";
const PHASE_OBSERVABILITY: &str = "observability";

pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    let settings = Settings::load(config_path)?;
    let log_identity = LogIdentity::new(
        &settings.app.name,
        env!("CARGO_PKG_VERSION"),
        settings.app.environment.as_str(),
    )
    .with_metric_names(YANG_SYSTEM_METRIC_NAMES);
    let mut telemetry = TelemetryRuntime::initialize(
        &settings.observability,
        &settings.logging.filter,
        &log_identity,
    )?;
    let shutdown_budget =
        ShutdownBudget::new(Duration::from_secs(settings.shutdown.total_timeout_seconds));
    tracing::info!(
        service = %log_identity.service,
        version = %log_identity.version,
        environment = %log_identity.environment,
        config = %config_path.display(),
        "开始启动系统"
    );
    let result = run_after_telemetry_initialized(
        &settings,
        log_identity,
        &mut telemetry,
        shutdown_budget.clone(),
    )
    .await;
    shutdown_budget.begin(TRIGGER_OPERATION_EXIT).await;
    let telemetry_result = shutdown_budget
        .run_phase(PHASE_OBSERVABILITY, telemetry.shutdown())
        .await
        .map_err(anyhow::Error::from);
    combine_operation_and_cleanup(result, telemetry_result, "可观测性运行时")
}

async fn run_after_telemetry_initialized(
    settings: &Settings,
    log_identity: LogIdentity,
    telemetry: &mut TelemetryRuntime,
    shutdown_budget: ShutdownBudget,
) -> anyhow::Result<()> {
    let token_manager = settings.token.build_manager()?;

    let mysql = Database::connect_with_config(&settings.mysql.url, settings.mysql_config())
        .await
        .context("连接 MySQL 失败")?;
    let initializer_mysql = Database::from_pool(mysql.pool().clone(), settings.mysql_config())
        .context("构造 schema 初始化数据库失败")?;
    let cache = RedisClient::connect_with_config(&settings.redis.url, settings.redis_config())
        .await
        .context("连接 Redis 失败")?;
    let authorization_cache =
        AuthorizationVersionCache::new(cache.clone(), settings.authorization.deployment.clone())
            .context("构建授权版本缓存失败")?;

    let step_up_manager = Arc::new(
        settings
            .step_up
            .build_manager()
            .context("构建 Step-up manager 失败")?,
    );
    let registration_email_sender = SmtpRegistrationEmailSender::new(&settings.email.smtp)
        .context("构建注册邮件 SMTP 投递器失败")?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .cache(cache)
            .token(token_manager)
            .extension(authorization_cache)
            .extension(step_up_manager)
            .extension(RegistrationEmailSenderHandle::new(
                registration_email_sender,
            ))
            .config(log_identity)
            .config(settings.email.verification.clone())
            .build()
            .context("构建应用 Tools 失败")?,
    );

    run_then_cleanup(
        run_after_tools_created(
            settings,
            initializer_mysql,
            Arc::clone(&tools),
            shutdown_budget.clone(),
            telemetry,
        ),
        tools.close(),
        &shutdown_budget,
    )
    .await
}

fn combine_operation_and_cleanup<T>(
    result: anyhow::Result<T>,
    cleanup_result: anyhow::Result<()>,
    cleanup_name: &str,
) -> anyhow::Result<T> {
    match (result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Err(operation_error), Err(cleanup_error)) => {
            tracing::error!(
                cleanup = cleanup_name,
                error = %cleanup_error,
                "业务阶段失败后，清理阶段也失败"
            );
            Err(operation_error)
        }
    }
}

/// 运行 Tools 构建后的完整启动与服务阶段。
///
/// 该函数整体位于 [`run_then_cleanup`] 的 operation 边界内，因此应用构建、schema、
/// 地址解析/绑定、服务运行失败以及正常退出都会进入同一个关闭出口。
async fn run_after_tools_created(
    settings: &Settings,
    initializer_mysql: Database,
    tools: Arc<Tools>,
    shutdown_budget: ShutdownBudget,
    telemetry: &mut TelemetryRuntime,
) -> anyhow::Result<()> {
    let application = build_app(Arc::clone(&tools), Arc::new(settings.security.clone()))
        .context("构建应用模块失败")?;

    let initializer = DatabaseInitializer::new(initializer_mysql);
    let definitions =
        crate::schema::definitions(&application.runtime).context("构建完整数据库 schema 失败")?;
    let schema = definitions.iter().collect::<Vec<_>>();
    let report = initializer
        .sync_table_definitions(&schema)
        .await
        .context("启动期同步数据库 schema 失败；请按错误中的表名、约束名和主键修复旧数据后重启")?;
    tracing::info!(
        tables = ?report.tables,
        changes = report.changes.len(),
        "数据库 schema 同步完成"
    );
    crate::audit::validate_schema(tools.mysql()?.pool())
        .await
        .context("启动期校验高权限审计表失败")?;
    crate::addon::admin::validate_system_owner_state(tools.mysql()?.pool())
        .await
        .context("启动期校验系统最终管理员状态失败")?;
    drop(initializer);

    let bind = settings.bind_addr()?;
    telemetry
        .start_management_server(
            settings.observability.metrics_bind_addr()?,
            Arc::clone(&tools),
        )
        .await?;
    let readiness_gate = telemetry.readiness_gate();
    let outbox_worker = AuthorizationOutboxWorker::start(&tools, settings.authorization.clone())
        .await
        .context("启动授权 Outbox Worker 失败")?;
    let runtime = Arc::new(application.runtime);
    let signal_budget = shutdown_budget.clone();
    let signal_readiness = readiness_gate.clone();
    readiness_gate.mark_ready();
    let shutdown_signal = async move {
        yang_base::lifecycle::wait_for_shutdown_signal().await;
        signal_readiness.mark_not_ready();
        signal_budget.begin(TRIGGER_SIGNAL).await;
    };
    let transport_config = AxumTransportConfig {
        max_body_bytes: settings.http.max_body_bytes,
        request_timeout: Some(Duration::from_secs(settings.http.request_timeout_seconds)),
        max_concurrency: Some(settings.http.max_concurrency),
        ..AxumTransportConfig::default()
    };
    let serve = async move {
        serve_with_shutdown(bind, runtime, transport_config, shutdown_signal)
            .await
            .context("HTTP 服务运行失败")
    };
    tokio::pin!(serve);
    let budget_started = shutdown_budget.wait_started();
    tokio::pin!(budget_started);

    let serve_result = tokio::select! {
        result = &mut serve => result,
        _ = &mut budget_started => {
            shutdown_budget
                .run_phase(PHASE_HTTP_DRAIN, &mut serve)
                .await
                .map_err(anyhow::Error::from)
        }
    };
    readiness_gate.mark_not_ready();
    shutdown_budget.begin(TRIGGER_OPERATION_EXIT).await;
    let shutdown_result = shutdown_budget
        .run_phase(PHASE_AUTHORIZATION_OUTBOX_WORKER, outbox_worker.shutdown())
        .await
        .map_err(anyhow::Error::from);
    match (serve_result, shutdown_result) {
        (Err(serve_error), Err(worker_error)) => {
            tracing::error!(
                error = %worker_error,
                "HTTP 服务退出失败后，授权 Outbox Worker 关闭也失败"
            );
            Err(serve_error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// 等待 operation 完成后无条件执行且仅执行一次 cleanup。
///
/// cleanup 与 operation 内部的关闭阶段共享同一截止时间；若两者都失败，保留最早的
/// operation 错误，同时记录 cleanup 错误。
async fn run_then_cleanup<T>(
    operation: impl Future<Output = anyhow::Result<T>>,
    cleanup: impl Future<Output = ()>,
    shutdown_budget: &ShutdownBudget,
) -> anyhow::Result<T> {
    let result = operation.await;
    shutdown_budget.begin(TRIGGER_OPERATION_EXIT).await;
    let cleanup_result = shutdown_budget
        .run_phase(PHASE_TOOLS_CLOSE, async {
            cleanup.await;
            Ok(())
        })
        .await
        .map_err(anyhow::Error::from);

    match (result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Err(operation_error), Err(cleanup_error)) => {
            tracing::error!(
                error = %cleanup_error,
                "业务阶段失败后，Tools 资源关闭也失败"
            );
            Err(operation_error)
        }
    }
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
            let shutdown_budget = ShutdownBudget::new(Duration::from_secs(1));
            let operation = async move {
                match failed_stage {
                    Some(stage) => Err(anyhow::anyhow!("{stage} failed")),
                    None => Ok("completed"),
                }
            };

            let result = run_then_cleanup(
                operation,
                async {
                    cleanup_calls.fetch_add(1, Ordering::SeqCst);
                },
                &shutdown_budget,
            )
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
        let shutdown_budget = ShutdownBudget::new(Duration::from_secs(1));
        let result: anyhow::Result<()> = run_then_cleanup(
            async { Err(anyhow::anyhow!("schema failed")) },
            tools.close(),
            &shutdown_budget,
        )
        .await;

        match result {
            Err(error) => assert_eq!(error.to_string(), "schema failed"),
            Ok(()) => panic!("业务失败必须保留"),
        }
        assert_eq!(tools.state(), ToolsState::Closed);
    }

    #[tokio::test]
    async fn cleanup_timeout_is_reported_for_success_but_does_not_hide_operation_failure() {
        let success_budget = ShutdownBudget::new(Duration::from_millis(20));
        let success_result = run_then_cleanup(
            async { Ok("completed") },
            tokio::time::sleep(Duration::from_millis(100)),
            &success_budget,
        )
        .await;
        let success_error = success_result
            .err()
            .unwrap_or_else(|| panic!("成功业务后的 cleanup 超时必须返回错误"));
        assert!(success_error.to_string().contains("tools_close"));

        let failure_budget = ShutdownBudget::new(Duration::from_millis(20));
        let failure_result: anyhow::Result<()> = run_then_cleanup(
            async { Err(anyhow::anyhow!("original failure")) },
            tokio::time::sleep(Duration::from_millis(100)),
            &failure_budget,
        )
        .await;
        let failure_error = failure_result
            .err()
            .unwrap_or_else(|| panic!("业务错误必须保留"));
        assert_eq!(failure_error.to_string(), "original failure");
    }
}
