//! 生产 JSON 日志与 Action 完成事件。

use crate::config::DeploymentEnvironment;
use anyhow::Context;
use async_trait::async_trait;
use std::time::Instant;
use tracing_subscriber::EnvFilter;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::router::{Middleware, Next};
use yang_base::tools::Tools;
use yang_base::BaseError;

const UNKNOWN_ENVIRONMENT: &str = "unknown";

/// 每条规范事件共享的低基数服务身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogIdentity {
    pub(crate) service: String,
    pub(crate) version: String,
    pub(crate) environment: String,
}

impl LogIdentity {
    pub(crate) fn new(service: &str, environment: DeploymentEnvironment) -> Self {
        Self {
            service: service.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            environment: environment.as_str().to_string(),
        }
    }

    pub(crate) fn from_tools(tools: &Tools) -> Self {
        tools
            .config::<Self>()
            .cloned()
            .unwrap_or_else(|_| Self::fallback())
    }

    fn fallback() -> Self {
        Self {
            service: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            environment: UNKNOWN_ENVIRONMENT.to_string(),
        }
    }
}

/// 位于 Addon 中间件链最外层，统一观察认证、租户解析和 Handler 的最终结果。
#[derive(Debug, Clone)]
pub(crate) struct ActionLogMiddleware {
    identity: LogIdentity,
}

impl ActionLogMiddleware {
    pub(crate) fn new(identity: LogIdentity) -> Self {
        Self { identity }
    }
}

#[async_trait]
impl Middleware for ActionLogMiddleware {
    async fn handle(
        &self,
        context: ActionContext,
        next: Next<'_>,
    ) -> Result<ApiResponse, BaseError> {
        let request_id = context.request_id();
        let operation = context
            .dispatch_target()
            .map(|(module, action)| format!("{module}.{action}"))
            .unwrap_or_else(|| "unknown.unknown".to_string());
        let started = Instant::now();
        let result = next.run(context).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let span = tracing::Span::current();
        span.record("duration_ms", duration_ms);

        match &result {
            Ok(response) if response.code == 0 => {
                span.record("result", "success");
                span.record("error_code", 0);
                tracing::info!(
                    service = %self.identity.service,
                    version = %self.identity.version,
                    environment = %self.identity.environment,
                    operation = %operation,
                    %request_id,
                    result = "success",
                    error_code = 0,
                    error = "",
                    duration_ms,
                    "Action 执行完成"
                );
            }
            Ok(response) => {
                span.record("result", "business_error");
                span.record("error_code", response.code);
                tracing::warn!(
                    service = %self.identity.service,
                    version = %self.identity.version,
                    environment = %self.identity.environment,
                    operation = %operation,
                    %request_id,
                    result = "business_error",
                    error_code = response.code,
                    error = %response.message,
                    duration_ms,
                    "Action 执行完成"
                );
            }
            Err(error) => {
                let error_code = error.code();
                span.record("result", "error");
                span.record("error_code", error_code);
                tracing::warn!(
                    service = %self.identity.service,
                    version = %self.identity.version,
                    environment = %self.identity.environment,
                    operation = %operation,
                    %request_id,
                    result = "error",
                    error_code,
                    error = %error,
                    duration_ms,
                    "Action 执行完成"
                );
            }
        }
        result
    }
}

pub(crate) fn init_tracing(filter: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter).context("logging.filter 无效")?;
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init()
        .map_err(|error| anyhow::anyhow!("初始化 tracing 失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_identity_is_stable_and_has_a_safe_unconfigured_fallback() {
        let identity = LogIdentity::new("system-api", DeploymentEnvironment::Test);
        assert_eq!(identity.service, "system-api");
        assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(identity.environment, "test");

        let tools = yang_base::tools::ToolsBuilder::new()
            .build()
            .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}"));
        let fallback = LogIdentity::from_tools(&tools);
        assert_eq!(fallback.service, env!("CARGO_PKG_NAME"));
        assert_eq!(fallback.environment, UNKNOWN_ENVIRONMENT);
    }
}
