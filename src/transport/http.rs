//! HTTP 传输接线：委托 yang-base `transport-axum` 适配器。
//!
//! 路由生成、body 限制、错误映射、request_id 透传、健康端点、CORS/超时/压缩
//! 与文件/重定向响应均由 `yang_base::transport::axum` 统一保证；应用侧只注入配置。

use crate::config::HttpSettings;
use anyhow::Context;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use yang_base::definition::BuiltApp;
use yang_base::transport::axum::AxumTransportConfig;

/// 启动 HTTP 服务直至关闭信号（优雅停机）。
pub async fn serve(
    bind: SocketAddr,
    app: Arc<BuiltApp>,
    settings: &HttpSettings,
) -> anyhow::Result<()> {
    yang_base::transport::axum::serve(bind, app, transport_config(settings))
        .await
        .context("HTTP 服务运行失败")
}

/// 使用调用方提供的关闭信号启动 HTTP 服务。
pub async fn serve_with_shutdown<S>(
    bind: SocketAddr,
    app: Arc<BuiltApp>,
    settings: &HttpSettings,
    shutdown_signal: S,
) -> anyhow::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    yang_base::transport::axum::serve_with_shutdown(
        bind,
        app,
        transport_config(settings),
        shutdown_signal,
    )
    .await
    .context("HTTP 服务运行失败")
}

fn transport_config(settings: &HttpSettings) -> AxumTransportConfig {
    AxumTransportConfig {
        max_body_bytes: settings.max_body_bytes,
        request_timeout: Some(Duration::from_secs(settings.request_timeout_seconds)),
        max_concurrency: Some(settings.max_concurrency),
        ..AxumTransportConfig::default()
    }
}
