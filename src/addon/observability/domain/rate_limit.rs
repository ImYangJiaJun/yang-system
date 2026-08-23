//! 前端错误上报的进程内限流，抑制已认证会话的日志与指标放大。

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use yang_base::BaseError;

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT_PER_USER: u32 = 30;
const MAX_TRACKED_USERS: usize = 4096;

#[derive(Debug)]
struct RateWindow {
    started_at: Instant,
    count: u32,
}

#[derive(Debug, Default)]
pub(in crate::addon::observability) struct FrontendErrorRateLimiter {
    windows: Mutex<HashMap<i64, RateWindow>>,
}

impl FrontendErrorRateLimiter {
    pub(in crate::addon::observability) async fn check(
        &self,
        actor_id: i64,
    ) -> Result<(), BaseError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
