//! 门户公开查询端点的 Redis 原子限速。
//!
//! # 为什么不复用 `yang_base` 的 `AuthRateLimiter`
//!
//! 那一个是「来源 IP + 业务身份」双维度、阈值在**构造期固定**（`AuthRateLimitConfig`）；
//! 门户的阈值是**每个门户一份**（`rate_limit_per_min`），维度也只有单键
//! （slug + IP）。硬套会需要「每个门户现造一个 limiter」，把配置对象当请求参数用。
//! 共用同一套 Lua 形态（`INCR` + 首次 `EXPIRE`，单脚本原子），但不共用一个类型。
//!
//! # 计数键不按部署命名空间隔离
//!
//! 部署是单实例（既有决策 A10），且这里的键只承载一个**上限**语义：跨部署碰撞
//! 的后果是限得更严，不是放行。为它引入 `AuthorizationVersionCache` 扩展依赖
//! 不划算——公开端点不该因为一个统计槽缺席而整个不可用。

use yang_base::action::ActionContext;
use yang_base::BaseError;

/// 计数窗口（秒）。
pub(crate) const WINDOW_SECONDS: u64 = 60;

/// 单键计数：自增；键是新建的才设过期；TTL 异常（无过期）时补设一次。
///
/// 返回 `{ 当前计数, 剩余 TTL }`。补设 TTL 那一段是必需的：只靠
/// `if current == 1 then EXPIRE` 时，若第一条 `EXPIRE` 与 `INCR` 之间进程被
/// 打断（或键由别处写入且未设过期），计数键会**永不过期**，该来源从此永久超限。
const COUNTER_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
if current == 1 then
    redis.call('EXPIRE', KEYS[1], tonumber(ARGV[1]))
end
local ttl = redis.call('TTL', KEYS[1])
if ttl == -1 then
    redis.call('EXPIRE', KEYS[1], tonumber(ARGV[1]))
    ttl = tonumber(ARGV[1])
end
return { current, ttl }
"#;

/// 门户查询的计数键。
///
/// 用**门户主键**而不是调用方传入的 slug：slug 查询在库层大小写不敏感
/// （`utf8mb4_unicode_ci`），原始 slug 的 `ABC`/`abc` 变体会命中同一门户却落入
/// 不同计数键，口令爆破预算可被成倍放大。主键是解析后的唯一事实。
pub(crate) fn query_key(portal_id: i64, client_ip: &str) -> String {
    format!("portal:query:{portal_id}:{client_ip}")
}

/// 超限判定（纯函数，便于单测；Redis 只负责给出计数与 TTL）。
fn verdict(current: i64, limit_per_minute: u32, ttl: i64) -> Result<(), BaseError> {
    if current <= i64::from(limit_per_minute) {
        return Ok(());
    }
    Err(BaseError::RateLimitExceeded {
        retry_after_seconds: u64::try_from(ttl).unwrap_or(1).max(1),
    })
}

/// 记一次查询并判定是否超限；超限返回 [`BaseError::RateLimitExceeded`]（HTTP 429）。
pub(crate) async fn check_query(
    ctx: &ActionContext,
    portal_id: i64,
    client_ip: &str,
    limit_per_minute: u32,
) -> Result<(), BaseError> {
    let cache = ctx.tools().cache()?;
    let script = cache.script(COUNTER_SCRIPT);
    let keys = [query_key(portal_id, client_ip)];
    let args = [WINDOW_SECONDS.to_string()];
    let (current, ttl): (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
    verdict(current, limit_per_minute, ttl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "需要独立 Redis DB 15"]
    async fn real_redis_counter_preserves_subsecond_window_and_repairs_missing_expiry(
    ) -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")?;
        anyhow::ensure!(
            url.trim_end_matches('/').ends_with("/15"),
            "只允许 Redis DB 15"
        );
        let cache =
            yang_db::RedisClient::connect_with_config(&url, yang_db::RedisConfig::default())
                .await?;
        let keys = [query_key(42, &uuid::Uuid::new_v4().to_string())];
        let args = [WINDOW_SECONDS.to_string()];
        let script = cache.script(&format!(
            "redis.call('SET', KEYS[1], 31, 'PX', 300)\n{COUNTER_SCRIPT}"
        ));
        let (count, ttl): (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
        assert_eq!((count, ttl), (32, 0), "窗口尾段不得续期");
        let script = cache.script(COUNTER_SCRIPT);
        for _ in 0..3 {
            let (_, ttl): (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
            assert_eq!(ttl, 0, "持续超限请求不得延长原窗口");
        }
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let (count, ttl): (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
        assert_eq!((count, ttl), (1, 60), "原窗口结束后应恢复额度");

        let script = cache.script(&format!("redis.call('SET', KEYS[1], 31)\n{COUNTER_SCRIPT}"));
        let (count, ttl): (i64, i64) = cache.eval_script(&script, &keys, &args).await?;
        assert_eq!((count, ttl), (32, 60), "无过期异常键仍须修复");
        let _: i64 = cache
            .eval_script(
                &cache.script("return redis.call('DEL', KEYS[1])"),
                &keys,
                &args,
            )
            .await?;
        Ok(())
    }

    #[test]
    fn the_key_shape_is_portal_id_then_client_ip() {
        assert_eq!(query_key(42, "203.0.113.7"), "portal:query:42:203.0.113.7");
    }

    #[test]
    fn the_limit_is_inclusive() {
        // 限速 30 表示**允许** 30 次：第 31 次才拒。
        assert!(verdict(30, 30, 42).is_ok());
        assert!(verdict(31, 30, 42).is_err());
    }

    #[test]
    fn an_exceeded_verdict_carries_a_usable_retry_after() {
        let error = verdict(31, 30, 42)
            .err()
            .unwrap_or_else(|| panic!("超限必须报错"));
        assert!(matches!(
            error,
            BaseError::RateLimitExceeded {
                retry_after_seconds: 42
            }
        ));
        // TTL 异常（键没有过期时间）时给 1 秒，不能给 0——0 会让客户端立刻重试。
        let error = verdict(31, 30, -1)
            .err()
            .unwrap_or_else(|| panic!("超限必须报错"));
        assert!(matches!(
            error,
            BaseError::RateLimitExceeded {
                retry_after_seconds: 1
            }
        ));
    }
}
