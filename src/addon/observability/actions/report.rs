//! 接收已认证浏览器的最小化错误指纹，不接收错误正文或堆栈。

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::{Action, BaseError};

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FrontendErrorKind {
    Api,
    Contract,
    Network,
    Promise,
    Runtime,
    Vue,
}

impl FrontendErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Contract => "contract",
            Self::Network => "network",
            Self::Promise => "promise",
            Self::Runtime => "runtime",
            Self::Vue => "vue",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FrontendErrorInput {
    event_id: String,
    kind: FrontendErrorKind,
    route: String,
    fingerprint: String,
    operation: Option<String>,
    related_request_id: Option<String>,
    status: Option<u16>,
    error_code: Option<u32>,
}

impl ParamInput for FrontendErrorInput {
    fn params() -> Params {
        Params::new()
    }
}

impl FrontendErrorInput {
    fn validate(&self) -> Result<(), BaseError> {
        validate_ascii(
            "event_id",
            &self.event_id,
            36,
            |index, byte| {
                matches!(index, 8 | 13 | 18 | 23) && byte == b'-'
                    || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit()
            },
            "必须是 UUID 形式的事件标识",
        )?;
        if self.route.is_empty()
            || self.route.len() > 64
            || !self.route.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
            })
        {
            return Err(invalid("route", "只能包含字母、数字、_、.、:、-"));
        }
        validate_ascii(
            "fingerprint",
            &self.fingerprint,
            16,
            |_index, byte| byte.is_ascii_hexdigit(),
            "必须是 16 位十六进制指纹",
        )?;
        if let Some(operation) = &self.operation {
            if operation.len() > 128
                || operation.is_empty()
                || !operation.as_bytes()[0].is_ascii_lowercase()
                || !operation.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'.' | b'-')
                })
            {
                return Err(invalid(
                    "operation",
                    "必须是最长 128 字节的稳定小写 operation id",
                ));
            }
        }
        if let Some(request_id) = &self.related_request_id {
            validate_ascii(
                "related_request_id",
                request_id,
                32,
                |_index, byte| byte.is_ascii_hexdigit(),
                "必须是 32 位十六进制 request id",
            )?;
        }
        if self
            .status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(invalid("status", "必须是 100..=599 的 HTTP 状态码"));
        }
        if self
            .error_code
            .is_some_and(|error_code| error_code > 999_999)
        {
            return Err(invalid("error_code", "必须小于等于 999999"));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct FrontendErrorAccepted {
    accepted: bool,
}

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT_PER_USER: u32 = 30;
const MAX_TRACKED_USERS: usize = 4096;

#[derive(Debug)]
struct RateWindow {
    started_at: Instant,
    count: u32,
}

#[derive(Debug, Default)]
struct FrontendErrorRateLimiter {
    windows: Mutex<HashMap<i64, RateWindow>>,
}

impl FrontendErrorRateLimiter {
    async fn check(&self, actor_id: i64) -> Result<(), BaseError> {
        let now = Instant::now();
        let mut windows = self.windows.lock().await;
        if windows.len() >= MAX_TRACKED_USERS && !windows.contains_key(&actor_id) {
            windows.retain(|_, window| now.duration_since(window.started_at) < RATE_LIMIT_WINDOW);
            if windows.len() >= MAX_TRACKED_USERS {
                return Err(BaseError::RateLimitExceeded {
                    retry_after_seconds: RATE_LIMIT_WINDOW.as_secs(),
                });
            }
        }
        let window = windows.entry(actor_id).or_insert(RateWindow {
            started_at: now,
            count: 0,
        });
        let elapsed = now.duration_since(window.started_at);
        if elapsed >= RATE_LIMIT_WINDOW {
            window.started_at = now;
            window.count = 0;
        }
        if window.count >= RATE_LIMIT_PER_USER {
            return Err(BaseError::RateLimitExceeded {
                retry_after_seconds: RATE_LIMIT_WINDOW.saturating_sub(elapsed).as_secs().max(1),
            });
        }
        window.count += 1;
        Ok(())
    }
}

#[derive(Action)]
#[action(
    name = "report_frontend_error",
    display_name = "上报前端错误",
    description = "记录已认证浏览器的无敏感正文错误指纹与后端请求关联",
    method = "POST",
    path = "/api/v1/observability/frontend-errors"
)]
struct FrontendErrorReportAction {
    rate_limiter: Arc<FrontendErrorRateLimiter>,
}

#[async_trait]
impl ActionHandler for FrontendErrorReportAction {
    type Input = FrontendErrorInput;
    type Output = FrontendErrorAccepted;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        input.validate()?;
        let actor_id = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))?
            .id;
        self.rate_limiter.check(actor_id).await?;
        let request_id = ctx.request_id();
        let related_request_id = input.related_request_id.as_deref().unwrap_or("");
        let operation = input.operation.as_deref().unwrap_or("");
        let status = input.status.unwrap_or(0);
        let error_code = input.error_code.unwrap_or(0);
        let linked = if input.related_request_id.is_some() {
            "true"
        } else {
            "false"
        };

        tracing::error!(
            event_type = "frontend.error",
            %request_id,
            %related_request_id,
            client_event_id = %input.event_id,
            frontend_kind = input.kind.as_str(),
            frontend_route = %input.route,
            frontend_operation = %operation,
            frontend_status = status,
            frontend_error_code = error_code,
            frontend_fingerprint = %input.fingerprint,
            actor_id,
            "前端错误上报"
        );
        metrics::counter!(
            "yang_system_frontend_errors_total",
            "kind" => input.kind.as_str(),
            "linked" => linked
        )
        .increment(1);

        Ok(FrontendErrorAccepted { accepted: true })
    }
}

pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module.native_action(FrontendErrorReportAction {
        rate_limiter: Arc::new(FrontendErrorRateLimiter::default()),
    })
}

fn validate_ascii(
    field: &str,
    value: &str,
    exact_length: usize,
    valid_byte: impl Fn(usize, u8) -> bool,
    message: &str,
) -> Result<(), BaseError> {
    if value.len() != exact_length
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| valid_byte(index, byte))
    {
        return Err(invalid(field, message));
    }
    Ok(())
}

fn invalid(field: &str, message: &str) -> BaseError {
    BaseError::ParamInvalid(field.to_string(), message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_input() -> FrontendErrorInput {
        FrontendErrorInput {
            event_id: "11111111-1111-4111-8111-111111111111".to_string(),
            kind: FrontendErrorKind::Api,
            route: "module-page".to_string(),
            fingerprint: "0123456789abcdef".to_string(),
            operation: Some("org.user.list".to_string()),
            related_request_id: Some("0123456789abcdef0123456789abcdef".to_string()),
            status: Some(503),
            error_code: Some(500001),
        }
    }

    #[test]
    fn accepts_only_bounded_correlation_fields() {
        assert!(valid_input().validate().is_ok());

        let mut invalid_request_id = valid_input();
        invalid_request_id.related_request_id = Some("request-id-from-client".to_string());
        assert!(invalid_request_id.validate().is_err());

        let mut invalid_operation = valid_input();
        invalid_operation.operation = Some("org/user?token=secret".to_string());
        assert!(invalid_operation.validate().is_err());

        let mut invalid_status = valid_input();
        invalid_status.status = Some(99);
        assert!(invalid_status.validate().is_err());
    }

    #[test]
    fn rejects_error_body_and_stack_fields_at_deserialization_boundary() {
        let payload = serde_json::json!({
            "event_id": "11111111-1111-4111-8111-111111111111",
            "kind": "runtime",
            "route": "module-page",
            "fingerprint": "0123456789abcdef",
            "message": "不得进入后端的错误正文",
            "stack": "不得进入后端的堆栈"
        });

        assert!(serde_json::from_value::<FrontendErrorInput>(payload).is_err());
    }

    #[tokio::test]
    async fn rate_limiter_bounds_authenticated_log_amplification() {
        let limiter = FrontendErrorRateLimiter::default();
        for _ in 0..RATE_LIMIT_PER_USER {
            assert!(limiter.check(7).await.is_ok());
        }
        assert!(matches!(
            limiter.check(7).await,
            Err(BaseError::RateLimitExceeded { .. })
        ));
        assert!(limiter.check(8).await.is_ok());
    }
}
