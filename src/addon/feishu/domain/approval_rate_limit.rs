//! 审批创建按应用共享的 Redis 匀速门控。
//!
//! 每次真实出站（含重试）才领取名额，不为未来排队者预订时间片。
//! Redis TIME 负责计算相对等待时间；本机时钟偏差不会提前放行。
//! 同一应用的所有部署必须共享同一个 Redis，独立 Redis 无法协调配额。

use std::time::Duration;
use yang_db::RedisClient;

use super::outbound::{FailureKind, OutboundFailure};

pub(crate) fn bucket_key(app_id: &str) -> String {
    format!("yang-system:feishu:app:{app_id}:approval_create_gate")
}

/// 向上取整保证实际速率不超过配置值；生产配置另校验官方 100/分钟上限。
pub(crate) fn min_interval_ms(rate_per_minute: u32) -> u64 {
    60_000_u64.div_ceil(u64::from(rate_per_minute.max(1)))
}

/// 最多等五秒；未取得名额时保留可重试结果，绝不带着过期预订出站。
pub(crate) async fn acquire_create_slot(
    redis: &RedisClient,
    app_id: &str,
    rate_per_minute: u32,
) -> Result<(), OutboundFailure> {
    let interval = min_interval_ms(rate_per_minute);
    let key = bucket_key(app_id);
    let script = redis.script(
        r#"
        local now_parts = redis.call('TIME')
        local now = tonumber(now_parts[1]) * 1000 + math.floor(tonumber(now_parts[2]) / 1000)
        local next_at = tonumber(redis.call('GET', KEYS[1]) or '0')
        if next_at > now then
            return next_at - now
        end
        local interval = tonumber(ARGV[1])
        redis.call('SET', KEYS[1], now + interval, 'PX', interval)
        return 0
        "#,
    );
    let pending = async {
        loop {
            let wait_ms: u64 = redis
                .eval_script(&script, std::slice::from_ref(&key), &[interval.to_string()])
                .await
                .map_err(|error| OutboundFailure {
                    kind: FailureKind::Retry {
                        retry_after_seconds: Some(1),
                    },
                    message: format!("取创建名额失败（Redis）：{error}"),
                })?;
            if wait_ms == 0 {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .map_err(|_| OutboundFailure {
            kind: FailureKind::Retry {
                retry_after_seconds: Some(1),
            },
            message: "等待审批创建名额超时，本次未出站".to_string(),
        })?
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_key_is_namespaced_by_app() {
        let key = bucket_key("cli_test");
        assert_eq!(key, "yang-system:feishu:app:cli_test:approval_create_gate");
        assert!(key.contains("feishu"));
        assert!(key.starts_with("yang-system:"), "{key}");
    }

    #[test]
    fn different_apps_get_different_keys() {
        assert_ne!(bucket_key("cli_one"), bucket_key("cli_two"));
    }

    #[test]
    fn interval_matches_the_rate() {
        // 间隔向上取整，不能让真实速率超过配置上限。
        assert_eq!(min_interval_ms(90), 667);
        assert_eq!(min_interval_ms(60), 1_000);
        assert_eq!(min_interval_ms(120), 500);
    }

    #[test]
    fn interval_never_reaches_zero() {
        // 极高速率（如 100000/分钟）算出 0ms 会让脚本里的 PEXPIRE 变成 0
        // （即删键），桶直接失效。
        for rate in [1, 60, 90, 1_000, 100_000, u32::MAX] {
            assert!(min_interval_ms(rate) >= 1, "速率 {rate} 的间隔必须 ≥1ms");
        }
    }

    #[test]
    fn zero_rate_is_treated_as_slowest_not_unlimited() {
        // 速率为 0 若解释成「不限速」，一次配置手误就会打爆配额。
        // 按最慢处理（1 次/分钟）比静默不限速安全。
        assert_eq!(min_interval_ms(0), 60_000);
    }

    #[test]
    fn effective_rate_stays_within_the_official_limit() {
        // 官方上限 100 次/分钟。配置 90 时实际放行速率必须 ≤ 100。
        let rate = 90_u32;
        let interval = min_interval_ms(rate);
        let effective_per_minute = 60_000 / interval;
        assert!(
            effective_per_minute <= 100,
            "实际速率 {effective_per_minute}/分钟 超过官方上限"
        );
    }

    #[tokio::test]
    #[ignore = "需要独立 Redis DB 15"]
    async fn real_redis_create_gate_keeps_spacing_when_waiters_outlive_key_ttl(
    ) -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")?;
        anyhow::ensure!(
            url.trim_end_matches('/').ends_with("/15"),
            "只允许 Redis DB 15"
        );
        let cache = RedisClient::connect_with_config(&url, yang_db::RedisConfig::default()).await?;
        let app_id = uuid::Uuid::new_v4().to_string();
        let started = std::time::Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        // 旧实现会一次预订 0..900ms，但键只活 200ms；后来的调用可抢走已预订时段。
        for _ in 0..10 {
            let cache = cache.clone();
            let app_id = app_id.clone();
            tasks.spawn(async move {
                acquire_create_slot(&cache, &app_id, 600).await?;
                Ok::<_, OutboundFailure>(started.elapsed())
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        for _ in 0..5 {
            let cache = cache.clone();
            let app_id = app_id.clone();
            tasks.spawn(async move {
                acquire_create_slot(&cache, &app_id, 600).await?;
                Ok::<_, OutboundFailure>(started.elapsed())
            });
        }
        let mut admitted = Vec::new();
        while let Some(result) = tasks.join_next().await {
            admitted.push(result??);
        }
        admitted.sort();
        assert_eq!(admitted.len(), 15);
        for pair in admitted.windows(2) {
            assert!(
                pair[1] - pair[0] >= std::time::Duration::from_millis(85),
                "并发等待者不得因键提前过期重叠放行：{admitted:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要独立 Redis DB 15"]
    async fn real_redis_create_gate_wait_is_bounded_and_does_not_reserve_future_slots(
    ) -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")?;
        anyhow::ensure!(
            url.trim_end_matches('/').ends_with("/15"),
            "只允许 Redis DB 15"
        );
        let cache = RedisClient::connect_with_config(&url, yang_db::RedisConfig::default()).await?;
        let app_id = uuid::Uuid::new_v4().to_string();
        acquire_create_slot(&cache, &app_id, 1).await?;
        let key = bucket_key(&app_id);
        let get = cache.script("return redis.call('GET', KEYS[1])");
        let before: String = cache
            .eval_script(&get, std::slice::from_ref(&key), &[])
            .await?;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(6),
            acquire_create_slot(&cache, &app_id, 1),
        )
        .await?;
        assert!(
            matches!(
                result,
                Err(OutboundFailure {
                    kind: FailureKind::Retry { .. },
                    ..
                })
            ),
            "过载只能保留可重试结果"
        );
        assert!(started.elapsed() < Duration::from_secs(6));
        let after: String = cache.eval_script(&get, &[key], &[]).await?;
        assert_eq!(before, after, "未放行的等待者不能预订或延长队列");
        Ok(())
    }
}
