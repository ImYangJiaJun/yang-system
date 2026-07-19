//! 认证入口的 Redis 原子限流。

use crate::config::SecuritySettings;
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

#[derive(Clone, Copy)]
pub(super) enum AuthOperation {
    Login,
    Register,
}

impl AuthOperation {
    fn key(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Register => "register",
        }
    }
}

#[derive(Clone)]
pub(super) struct AuthRateLimiter {
    window_seconds: u64,
    ip_attempts: u64,
    username_attempts: u64,
}

impl AuthRateLimiter {
    pub(super) fn new(settings: &SecuritySettings) -> Self {
        Self {
            window_seconds: settings.auth_rate_limit_window_seconds,
            ip_attempts: settings.auth_rate_limit_ip_attempts,
            username_attempts: settings.auth_rate_limit_username_attempts,
        }
    }

    pub(super) async fn check(
        &self,
        ctx: &ActionContext,
        operation: AuthOperation,
        username: &str,
    ) -> Result<(), BaseError> {
        let source = ctx
            .request_meta
            .peer_addr
            .map(|address| address.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let prefix = format!("yang-system:auth-rate:{}", operation.key());
        let keys = [
            format!("{prefix}:ip:{source}"),
            format!("{prefix}:username:{username}"),
        ];
        let args = [
            self.window_seconds.to_string(),
            self.ip_attempts.to_string(),
            self.username_attempts.to_string(),
        ];
        let cache = ctx.tools().cache()?;
        let script = cache.script(RATE_LIMIT_SCRIPT);
        let (exceeded, retry_after): (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
        rate_limit_result(exceeded, retry_after)
    }
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
    }
}
