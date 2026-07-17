//! HTTP 传输接线：委托 yang-base `transport-axum` 适配器。
//!
//! 路由生成、body 限制、错误映射、request_id 透传、健康端点、CORS/超时/压缩
//! 与文件/重定向响应均由 `yang_base::transport::axum` 统一保证；应用侧只注入配置。

use anyhow::Context;
use std::net::SocketAddr;
use std::sync::Arc;
use yang_base::definition::BuiltApp;
use yang_base::transport::axum::AxumTransportConfig;

/// 启动 HTTP 服务直至关闭信号（优雅停机）。
pub async fn serve(
    bind: SocketAddr,
    app: Arc<BuiltApp>,
    max_body_bytes: usize,
) -> anyhow::Result<()> {
    let config = AxumTransportConfig {
        max_body_bytes,
        ..AxumTransportConfig::default()
    };
    yang_base::transport::axum::serve(bind, app, config)
        .await
        .context("HTTP 服务运行失败")
}
