//! 认证入口的 Redis 原子限流。

use crate::config::SecuritySettings;
use crate::security::CLIENT_IP_META_KEY;
use std::borrow::Cow;
use yang_base::action::ActionContext;
use yang_base::BaseError;

const RATE_LIMIT_SCRIPT: &str = r#"
local exceeded = 0
local retry_after = 0
local window = tonumber(ARGV[1])

for index, key in ipairs(KEYS) do
    local current = redis.call('INCR', key)
    if current == 1 then
        redis.call('EXPIRE', key, window)
    end

    local ttl = redis.call('TTL', key)
    if ttl < 1 then
        redis.call('EXPIRE', key, window)
        ttl = window
    end

    local limit = tonumber(ARGV[index + 1])
    if current > limit then
        exceeded = 1
        if ttl > retry_after then
            retry_after = ttl
        end
    end
end

return { exceeded, retry_after }
"#;

const FAILURE_COUNT_SCRIPT: &str = r#"
local exceeded = 0
local retry_after = 0
local window = tonumber(ARGV[1])

for index, key in ipairs(KEYS) do
    local current = redis.call('INCR', key)
    if current == 1 then
        redis.call('EXPIRE', key, window)
    end

    local ttl = redis.call('TTL', key)
    if ttl < 1 then
        redis.call('EXPIRE', key, window)
        ttl = window
    end

    local limit = tonumber(ARGV[index + 1])
    if current > limit then
        exceeded = 1
        if ttl > retry_after then
            retry_after = ttl
        end
    end
end

return { exceeded, retry_after }
"#;

#[derive(Clone, Copy)]
pub(crate) enum AuthOperation {
    ChangePassword,
    PasswordResetCreate,
    PasswordResetConsume,
    Login,
    Register,
    StepUpComplete,
}

impl AuthOperation {
    fn key(self) -> &'static str {
        match self {
            Self::ChangePassword => "change-password",
            Self::PasswordResetCreate => "password-reset-create",
            Self::PasswordResetConsume => "password-reset-consume",
            Self::Login => "login",
            Self::Register => "register",
            Self::StepUpComplete => "step-up-complete",
        }
    }

    fn identity_key(self) -> &'static str {
        match self {
            Self::ChangePassword => "user",
            Self::PasswordResetCreate => "actor-target",
            Self::PasswordResetConsume => "fingerprint",
            Self::Login | Self::Register | Self::StepUpComplete => "username",
        }
    }
}

#[derive(Clone)]
pub(crate) struct AuthRateLimiter {
    window_seconds: u64,
    ip_attempts: u64,
    username_attempts: u64,
}

impl AuthRateLimiter {
    pub(crate) fn new(settings: &SecuritySettings) -> Self {
        Self {
            window_seconds: settings.auth_rate_limit_window_seconds,
            ip_attempts: settings.auth_rate_limit_ip_attempts,
            username_attempts: settings.auth_rate_limit_username_attempts,
        }
    }

    pub(crate) async fn check(
        &self,
        ctx: &ActionContext,
        operation: AuthOperation,
        identity: &str,
    ) -> Result<(), BaseError> {
        let source = client_ip_identity(ctx);
        let prefix = format!("yang-system:auth-rate:{}", operation.key());
        let keys = [
            format!("{prefix}:ip:{source}"),
            format!("{prefix}:{}:{identity}", operation.identity_key()),
        ];
        let args = [
            self.window_seconds.to_string(),
            self.ip_attempts.to_string(),
            self.username_attempts.to_string(),
        ];
        let cache = ctx.tools().cache()?;
        let script = cache.script(RATE_LIMIT_SCRIPT);
        let decision: Result<(i64, i64), _> = cache.eval_script(&script, &keys, &args).await;
        match decision {
            Ok((exceeded, retry_after)) => {
                let result = rate_limit_result(exceeded, retry_after);
                metrics::counter!(
                    "yang_system_auth_rate_limit_total",
                    "operation" => operation.key(),
                    "result" => if result.is_ok() { "allowed" } else { "limited" }
                )
                .increment(1);
                result
            }
            Err(error) => {
                metrics::counter!(
                    "yang_system_auth_rate_limit_total",
                    "operation" => operation.key(),
                    "result" => "unavailable"
                )
                .increment(1);
                Err(error.into())
            }
        }
    }

    pub(crate) async fn record_failure(
        &self,
        ctx: &ActionContext,
        operation: AuthOperation,
        identity: &str,
    ) -> Result<(), BaseError> {
        let keys = self.failure_keys(ctx, operation, identity);
        let args = [
            self.window_seconds.to_string(),
            self.ip_attempts.to_string(),
            self.username_attempts.to_string(),
        ];
        let cache = ctx.tools().cache()?;
        let script = cache.script(FAILURE_COUNT_SCRIPT);
        let decision: (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
        rate_limit_result(decision.0, decision.1)
    }

    pub(crate) async fn clear_failures(
        &self,
        ctx: &ActionContext,
        operation: AuthOperation,
        identity: &str,
    ) -> Result<(), BaseError> {
        let keys = self.failure_keys(ctx, operation, identity);
        ctx.tools().cache()?.del(&keys).await?;
        Ok(())
    }

    fn failure_keys(
        &self,
        ctx: &ActionContext,
        operation: AuthOperation,
        identity: &str,
    ) -> [String; 2] {
        let source = client_ip_identity(ctx);
        let prefix = format!("yang-system:auth-failure:{}", operation.key());
        [
            format!("{prefix}:ip:{source}"),
            format!("{prefix}:{}:{identity}", operation.identity_key()),
        ]
    }
}

pub(crate) fn client_ip_identity(ctx: &ActionContext) -> Cow<'_, str> {
    ctx.request_meta
        .extensions
        .get(CLIENT_IP_META_KEY)
        .map(|value| Cow::Borrowed(value.as_str()))
        .or_else(|| {
            ctx.request_meta
                .peer_addr
                .map(|address| Cow::Owned(address.ip().to_string()))
        })
        .unwrap_or(Cow::Borrowed("unknown"))
}

fn rate_limit_result(exceeded: i64, retry_after: i64) -> Result<(), BaseError> {
    if exceeded == 0 {
        return Ok(());
    }
    Err(BaseError::RateLimitExceeded {
        retry_after_seconds: u64::try_from(retry_after).unwrap_or(1).max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use yang_base::action::{Request, RequestMeta};
    use yang_base::tools::ToolsBuilder;

    #[test]
    fn maps_redis_decision_to_standard_rate_limit_error() {
        assert!(rate_limit_result(0, 0).is_ok());
        assert!(matches!(
            rate_limit_result(1, 17),
            Err(BaseError::RateLimitExceeded {
                retry_after_seconds: 17
            })
        ));
        assert!(matches!(
            rate_limit_result(1, -1),
            Err(BaseError::RateLimitExceeded {
                retry_after_seconds: 1
            })
        ));
    }

    #[test]
    fn operation_keys_are_isolated() {
        assert_ne!(AuthOperation::Login.key(), AuthOperation::Register.key());
        assert_ne!(
            AuthOperation::ChangePassword.key(),
            AuthOperation::Login.key()
        );
        assert_eq!(AuthOperation::ChangePassword.identity_key(), "user");
        assert_eq!(AuthOperation::Login.identity_key(), "username");
        assert_ne!(
            AuthOperation::StepUpComplete.key(),
            AuthOperation::Login.key()
        );
        assert_eq!(AuthOperation::StepUpComplete.identity_key(), "username");
    }

    #[test]
    fn rate_limit_identity_prefers_trusted_transport_extension() {
        let tools = ToolsBuilder::new()
            .build()
            .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}"));
        let peer = "10.0.0.2:443"
            .parse::<SocketAddr>()
            .unwrap_or_else(|error| panic!("测试对端地址应有效: {error}"));
        let mut context =
            ActionContext::new(Request::new(serde_json::Value::Null), Arc::new(tools))
                .with_request_meta(RequestMeta::new().with_peer_addr(peer));
        context
            .request_meta
            .extensions
            .insert(CLIENT_IP_META_KEY.to_string(), "198.51.100.7".to_string());

        assert_eq!(client_ip_identity(&context), "198.51.100.7");
        context.request_meta.extensions.clear();
        assert_eq!(client_ip_identity(&context), "10.0.0.2");
        context.request_meta.peer_addr = None;
        assert_eq!(client_ip_identity(&context), "unknown");
    }

    #[test]
    fn step_up_failure_keys_are_separate_from_login_attempt_keys() {
        let settings = SecuritySettings {
            argon2_max_concurrency: 1,
            auth_rate_limit_window_seconds: 60,
            auth_rate_limit_ip_attempts: 10,
            auth_rate_limit_username_attempts: 5,
            password_reset_ttl_seconds: 900,
            issue_refresh_credential_version: true,
            trusted_proxy_cidrs: Vec::new(),
        };
        let limiter = AuthRateLimiter::new(&settings);
        let tools = ToolsBuilder::new()
            .build()
            .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}"));
        let context = ActionContext::new(Request::new(serde_json::Value::Null), Arc::new(tools));
        let keys = limiter.failure_keys(&context, AuthOperation::StepUpComplete, "alice");

        assert_eq!(
            keys,
            [
                "yang-system:auth-failure:step-up-complete:ip:unknown".to_string(),
                "yang-system:auth-failure:step-up-complete:username:alice".to_string(),
            ]
        );
        assert!(keys.iter().all(|key| !key.contains("auth-rate:login")));
    }
}
