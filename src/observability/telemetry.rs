//! Prometheus 与 OpenTelemetry 的单一进程级运行时。

use super::logging::LogIdentity;
use crate::config::ObservabilitySettings;
use anyhow::Context;
use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use yang_base::tools::Tools;

const HISTOGRAM_BUCKETS_SECONDS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub(crate) struct TelemetryRuntime {
    metrics_handle: Option<PrometheusHandle>,
    metrics_server: Option<MetricsServer>,
    tracer_provider: Option<SdkTracerProvider>,
    trace_export_timeout: Duration,
}

impl TelemetryRuntime {
    pub(crate) fn initialize(
        settings: &ObservabilitySettings,
        filter: &str,
        identity: &LogIdentity,
    ) -> anyhow::Result<Self> {
        let filter = EnvFilter::try_new(filter).context("logging.filter 无效")?;
        let trace_export_timeout = Duration::from_secs(settings.traces_export_timeout_seconds);

        let tracer_provider = if settings.traces_enabled {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(settings.traces_otlp_endpoint.clone())
                .with_timeout(trace_export_timeout)
                .build()
                .context("构建 OTLP trace exporter 失败")?;
            let resource = Resource::builder_empty()
                .with_service_name(identity.service.clone())
                .with_attributes([
                    KeyValue::new("service.version", identity.version.clone()),
                    KeyValue::new("deployment.environment.name", identity.environment.clone()),
                ])
                .build();
            Some(
                SdkTracerProvider::builder()
                    .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                        settings.traces_sample_ratio,
                    ))))
                    .with_batch_exporter(exporter)
                    .with_resource(resource)
                    .build(),
            )
        } else {
            None
        };

        let otel_layer = tracer_provider.as_ref().map(|provider| {
            tracing_opentelemetry::layer().with_tracer(provider.tracer(identity.service.clone()))
        });
        let json_layer = tracing_subscriber::fmt::layer()
            .json()
            .flatten_event(true)
            .with_ansi(false);
        tracing_subscriber::registry()
            .with(filter)
            .with(otel_layer)
            .with(json_layer)
            .try_init()
            .map_err(|error| anyhow::anyhow!("初始化 tracing 失败: {error}"))?;

        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let metrics_handle = if settings.metrics_enabled {
            let handle = PrometheusBuilder::new()
                .set_buckets(HISTOGRAM_BUCKETS_SECONDS)
                .context("配置 Prometheus histogram buckets 失败")?
                .install_recorder()
                .context("安装 Prometheus recorder 失败")?;
            metrics::gauge!(
                "yang_system_build_info",
                "service" => identity.service.clone(),
                "version" => identity.version.clone(),
                "environment" => identity.environment.clone()
            )
            .set(1.0);
            Some(handle)
        } else {
            None
        };

        Ok(Self {
            metrics_handle,
            metrics_server: None,
            tracer_provider,
            trace_export_timeout,
        })
    }

    pub(crate) async fn start_metrics_server(
        &mut self,
        bind: SocketAddr,
        tools: Arc<Tools>,
    ) -> anyhow::Result<()> {
        let Some(handle) = self.metrics_handle.clone() else {
            return Ok(());
        };
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .with_context(|| format!("绑定 Prometheus 监听地址失败: {bind}"))?;
        let local_addr = listener
            .local_addr()
            .context("读取 Prometheus 实际监听地址失败")?;
        let state = MetricsState { handle, tools };
        let router = Router::new()
            .route("/metrics", get(scrape_metrics))
            .with_state(state);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .context("Prometheus HTTP 服务退出失败")
        });
        self.metrics_server = Some(MetricsServer {
            local_addr,
            shutdown,
            task,
        });
        tracing::info!(bind = %local_addr, path = "/metrics", "Prometheus 指标端点已启动");
        Ok(())
    }

    pub(crate) async fn shutdown(&mut self) -> anyhow::Result<()> {
        if let Some(server) = self.metrics_server.take() {
            server.shutdown().await?;
        }
        if let Some(provider) = self.tracer_provider.take() {
            provider
                .shutdown_with_timeout(self.trace_export_timeout)
                .map_err(|error| {
                    anyhow::anyhow!("关闭 OpenTelemetry tracer provider 失败: {error}")
                })?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn metrics_local_addr(&self) -> Option<SocketAddr> {
        self.metrics_server.as_ref().map(|server| server.local_addr)
    }
}

#[derive(Clone)]
struct MetricsState {
    handle: PrometheusHandle,
    tools: Arc<Tools>,
}

async fn scrape_metrics(State(state): State<MetricsState>) -> impl IntoResponse {
    record_pool_metrics(&state.tools);
    state.handle.run_upkeep();
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        state.handle.render(),
    )
}

fn record_pool_metrics(tools: &Tools) {
    if let Ok(database) = tools.mysql() {
        let status = database.pool_status();
        record_pool_status("mysql", status);
    }
    if let Ok(cache) = tools.cache() {
        let status = cache.pool_status();
        record_pool_status("redis", status);
    }
}

fn record_pool_status(resource: &'static str, status: yang_db::PoolStatus) {
    for (state, value) in [
        ("max", status.max_size),
        ("open", status.size),
        ("available", status.available),
        ("waiting", status.waiting),
    ] {
        metrics::gauge!(
            "yang_system_resource_pool_connections",
            "resource" => resource,
            "state" => state
        )
        .set(value as f64);
    }
}

struct MetricsServer {
    local_addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl MetricsServer {
    async fn shutdown(self) -> anyhow::Result<()> {
        tracing::info!(bind = %self.local_addr, "开始关闭 Prometheus 指标端点");
        let _ = self.shutdown.send(());
        self.task
            .await
            .context("等待 Prometheus HTTP 服务退出失败")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeploymentEnvironment;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use yang_base::tools::ToolsBuilder;

    #[tokio::test]
    async fn metrics_endpoint_exports_build_info_and_tracer_shuts_down() {
        let settings = ObservabilitySettings {
            metrics_enabled: true,
            metrics_bind: "127.0.0.1:0".to_string(),
            traces_enabled: true,
            traces_otlp_endpoint: "http://127.0.0.1:4317".to_string(),
            traces_sample_ratio: 0.1,
            traces_export_timeout_seconds: 1,
        };
        let identity = LogIdentity::new("telemetry-test", DeploymentEnvironment::Test);
        let mut runtime = TelemetryRuntime::initialize(&settings, "off", &identity)
            .unwrap_or_else(|error| panic!("测试可观测性运行时应初始化: {error:#}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .build()
                .unwrap_or_else(|error| panic!("空测试 Tools 应构建成功: {error}")),
        );
        runtime
            .start_metrics_server(
                settings
                    .metrics_bind_addr()
                    .unwrap_or_else(|error| panic!("测试监听地址应有效: {error}")),
                tools,
            )
            .await
            .unwrap_or_else(|error| panic!("测试 Prometheus 端点应启动: {error:#}"));
        let address = runtime
            .metrics_local_addr()
            .unwrap_or_else(|| panic!("启动后应公开实际测试监听地址"));

        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .unwrap_or_else(|error| panic!("应连接测试 Prometheus 端点: {error}"));
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap_or_else(|error| panic!("应写入测试 scrape 请求: {error}"));
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .unwrap_or_else(|error| panic!("应读取测试 scrape 响应: {error}"));

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("text/plain; version=0.0.4"));
        assert!(response.contains("yang_system_build_info"));
        assert!(response.contains("service=\"telemetry-test\""));
        runtime
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("测试可观测性运行时应优雅关闭: {error:#}"));
    }
}
