//! 批量审批写请求的进程内全局限速。
//!
//! 飞书对 `pass`/`refuse`/`forward` 的配额是 100 次/分钟，对
//! `rollback`/`add_sign` 是 1000 次/分钟。这里用两个容量为 1 的令牌桶做
//! 匀速放行：容量取 1 可以保证任意 60 秒窗口内不会超发，而不是先打满
//! 100 次再等一分钟。桶是进程级静态状态，与设计文档一致；多实例部署前
//! 需要换成 Redis 共享桶。

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const FAST_RATE_PER_MINUTE: u32 = 100;
const SLOW_RATE_PER_MINUTE: u32 = 1000;

struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new() -> Self {
        Self {
            tokens: 1.0,
            last: Instant::now(),
        }
    }

    fn refill(&mut self, rate_per_minute: u32, now: Instant) {
        let rate_per_second = f64::from(rate_per_minute.max(1)) / 60.0;
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate_per_second).min(1.0);
        self.last = now;
    }

    /// 取一个令牌；取不到时返回距离下一个令牌的等待时间。
    fn try_take(&mut self, rate_per_minute: u32, now: Instant) -> Option<Duration> {
        self.refill(rate_per_minute, now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return None;
        }
        let rate_per_second = f64::from(rate_per_minute.max(1)) / 60.0;
        let wait = Duration::from_secs_f64((1.0 - self.tokens) / rate_per_second);
        Some(wait.max(Duration::from_millis(1)))
    }
}

static FAST_BUCKET: LazyLock<Mutex<TokenBucket>> = LazyLock::new(|| Mutex::new(TokenBucket::new()));
static SLOW_BUCKET: LazyLock<Mutex<TokenBucket>> = LazyLock::new(|| Mutex::new(TokenBucket::new()));

pub(crate) fn rate_for_action(action: &str) -> u32 {
    match action {
        "pass" | "refuse" | "forward" => FAST_RATE_PER_MINUTE,
        "rollback" | "add_sign" => SLOW_RATE_PER_MINUTE,
        // 未知动作不应走到这里；按最严格的档位处理，避免配置/枚举漂移放大配额。
        _ => FAST_RATE_PER_MINUTE,
    }
}

fn bucket_for(rate_per_minute: u32) -> &'static Mutex<TokenBucket> {
    if rate_per_minute == SLOW_RATE_PER_MINUTE {
        &SLOW_BUCKET
    } else {
        &FAST_BUCKET
    }
}

/// 在真正出站前领取一个全局令牌；等待期间不持有任何数据库锁。
pub(crate) async fn acquire(action: &str) {
    let rate = rate_for_action(action);
    loop {
        let wait = {
            let mut bucket = bucket_for(rate)
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            bucket.try_take(rate, Instant::now())
        };
        match wait {
            None => return,
            Some(delay) => tokio::time::sleep(delay).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_actions_map_to_the_official_rates() {
        for action in ["pass", "refuse", "forward"] {
            assert_eq!(rate_for_action(action), 100);
        }
        for action in ["rollback", "add_sign"] {
            assert_eq!(rate_for_action(action), 1000);
        }
    }

    #[test]
    fn unknown_action_is_treated_as_the_strict_rate() {
        assert_eq!(rate_for_action("unknown"), 100);
    }

    #[test]
    fn bucket_allows_one_immediate_token_then_waits_for_refill() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new();
        assert!(bucket.try_take(100, now).is_none());
        let wait = bucket.try_take(100, now);
        assert!(wait.is_some(), "第二个令牌必须等待补发");
        let wait = wait.unwrap_or_default();
        assert!(
            wait >= Duration::from_millis(590) && wait <= Duration::from_millis(610),
            "100/min 的补发间隔应约 600ms，实际 {wait:?}"
        );
    }

    #[test]
    fn slow_bucket_refills_at_sixty_millisecond_intervals() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new();
        assert!(bucket.try_take(1000, now).is_none());
        let wait = bucket.try_take(1000, now);
        assert!(wait.is_some(), "第二个令牌必须等待补发");
        let wait = wait.unwrap_or_default();
        assert!(
            wait >= Duration::from_millis(55) && wait <= Duration::from_millis(65),
            "1000/min 的补发间隔应约 60ms，实际 {wait:?}"
        );
    }

    #[test]
    fn refill_does_not_accumulate_more_than_one_token() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new();
        bucket.last = now - Duration::from_secs(3600);
        bucket.refill(100, now);
        assert_eq!(bucket.tokens, 1.0);
    }
}
