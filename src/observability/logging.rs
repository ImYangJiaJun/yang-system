//! 生产 JSON 日志与 Action 完成事件。

use crate::config::DeploymentEnvironment;
use async_trait::async_trait;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt;
use std::collections::HashMap;
use std::time::Instant;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
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
        let request_span = tracing::info_span!(
            "action.request",
            operation = %operation,
            %request_id,
            result = tracing::field::Empty,
            error_code = tracing::field::Empty,
            duration_ms = tracing::field::Empty
        );
        let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&RequestHeaderExtractor(&context.request.headers))
        });
        if parent.span().span_context().is_valid() {
            request_span
                .set_parent(parent)
                .unwrap_or_else(|error| tracing::debug!(%error, "设置远端 trace parent 失败"));
        }

        async move {
            let started = Instant::now();
            let result = next.run(context).await;
            let elapsed = started.elapsed();
            let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            let span = tracing::Span::current();
            span.record("duration_ms", duration_ms);

            let result_label = match &result {
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
                    "success"
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
                    "business_error"
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
                    "error"
                }
            };
            metrics::counter!(
                "yang_system_action_requests_total",
                "operation" => operation.clone(),
                "result" => result_label
            )
            .increment(1);
            metrics::histogram!(
                "yang_system_action_duration_seconds",
                "operation" => operation,
                "result" => result_label
            )
            .record(elapsed.as_secs_f64());
            result
        }
        .instrument(request_span)
        .await
    }
}

struct RequestHeaderExtractor<'a>(&'a HashMap<String, String>);

impl Extractor for RequestHeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry_sdk::propagation::TraceContextPropagator;

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

    #[test]
    fn request_headers_preserve_w3c_trace_context_without_logging_values() {
        let headers = HashMap::from([(
            "traceparent".to_string(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
        )]);
        let context = TraceContextPropagator::new().extract(&RequestHeaderExtractor(&headers));
        let span_context = context.span().span_context().clone();

        assert!(span_context.is_remote());
        assert_eq!(
            span_context.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
    }
}
