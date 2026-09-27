//! 审批实例创建的全局令牌桶。
//!
//! # 为什么需要它
//!
//! 创建审批实例接口的限速是 **100 次/分钟**。批量处理（页面按钮）会连续创建
//! 几十到几百个实例，不限速就会成片撞限流——而 `1395001` 虽被判为可重试，
//! 反复退避重试本身也在消耗配额，形成「越重试越慢」的恶性循环。
//!
//! # 为什么桶放 Redis 而不是进程内存
//!
//! 这不是为了跨实例互斥（部署是单实例，既有决策 A10），而是**成本控制**：
//! 进程内的桶在每次重启后清零，于是「重启 → 桶满 → 瞬间打爆配额」。
//! 放 Redis 让配额跨重启守恒。
//!
//! # 为什么按固定间隔匀速放行
//!
//! 用「固定间隔 + 下次可取的绝对时间」而不是「每 60 秒重置计数」：后者会让
//! 一分钟内的前 90 次立刻放行、后 60 秒全在等，实际速率是允许值的数倍。
//! 匀速放行把瞬时速率也压在配额内。
//!
//! 时间基准取 **Redis 服务器的 `TIME`** 而不是本机时间：多实例（蓝绿切换瞬间
//! 新旧共存）时本机时钟可能有偏差，而配额的权威方是 Redis。

#![allow(dead_code)] // 限速先落地并自带测试；消费者（派发 worker）在后续任务接入。

use yang_db::RedisClient;

use super::outbound::OutboundFailure;

/// 令牌桶的 Redis 键。
///
/// 用单个键存「下次可取的时间戳（毫秒）」——这是最省的表示：桶容量恒为 1，
/// 不需要存令牌数。
pub(crate) fn bucket_key(deployment: &str) -> String {
    format!("yang-system:{deployment}:feishu:approval_create_gate")
}

/// 两次创建之间的最小间隔（毫秒）。
///
/// 由速率算出：`60000 / rate_per_minute`（整数除法，向下取整）。
/// 速率 90/分钟 → 666ms（实际放行 90.09 次/分钟，仍在官方 100 的配额内）。
///
/// **向下取整的方向是刻意的**：间隔偏小意味着实际速率略高于配置值，但那仍在
/// 配置的意图之内（配置本身就留了余量）；反过来向上取整会让实际速率低于承诺，
/// 排查「为什么这么慢」时会找不到原因。
pub(crate) fn min_interval_ms(rate_per_minute: u32) -> u64 {
    if rate_per_minute == 0 {
        // 速率为 0 表示「不限制」在实际运维里没有意义，反而会让部署卡死。
        // 按 1 次/分钟处理——比静默不限速安全。
        return 60_000;
    }
    (60_000 / u64::from(rate_per_minute)).max(1)
}

/// 取一个创建名额，必要时等待。
///
/// 成功返回即表示「现在可以发起创建」。并发调用者由 Lua 脚本串行化：
/// 每个调用者拿到一个独占的时间片，不会两个调用者同时放行。
pub(crate) async fn acquire_create_slot(
    redis: &RedisClient,
    deployment: &str,
    rate_per_minute: u32,
) -> Result<(), OutboundFailure> {
    let interval = min_interval_ms(rate_per_minute);
    let key = bucket_key(deployment);

    // 脚本返回「本次调用应等到的时间戳（毫秒）」。
    //
    // 用 Redis 的 `TIME` 取当前时间：多实例时钟偏差时以 Redis 为准。
    // 脚本内不做 sleep（会阻塞 Redis 单线程），只算出一个目标时刻，
    // 由调用方在本地等。
    let script = redis.script(
        r#"
        local key = KEYS[1]
        local interval = tonumber(ARGV[1])

        local now_parts = redis.call('TIME')
        local now = tonumber(now_parts[1]) * 1000 + math.floor(tonumber(now_parts[2]) / 1000)

        local next_at = tonumber(redis.call('GET', key) or '0')
        local target = math.max(now, next_at)

        redis.call('SET', key, target + interval)
        -- 键的 TTL 只需覆盖一个间隔再多一点，避免残留
        redis.call('PEXPIRE', key, interval * 2)

        return target
        "#,
    );

    let target_ms: i64 = redis
        .eval_script(&script, &[key], &[interval.to_string()])
        .await
        .map_err(|error| OutboundFailure {
            kind: super::outbound::FailureKind::Retry {
                retry_after_seconds: None,
            },
            message: format!("取创建名额失败（Redis）：{error}"),
        })?;

    // 本地等待到目标时刻。时间基准用 Redis 的时钟——本机与 Redis 有偏差时，
    // 等久了只是慢一点，等少了会被限流，两者都不致命。
    let now_ms = local_now_ms();
    if target_ms > now_ms {
        let wait = (target_ms - now_ms) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    }
    Ok(())
}

/// 本机当前时间（自 epoch 起的毫秒）。
///
/// 只用于算出「还要等多久」，权威时刻来自 Redis。
fn local_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        // 系统时钟早于 epoch 是异常状态，但不应让审批派发崩掉——按 0 处理，
        // 最坏情况是多等一会儿。
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_key_is_namespaced_by_deployment() {
        // 与 tenant_token 的键同源命名空间，避免不同部署互相干扰。
        let key = bucket_key("prod");
        assert!(key.contains("prod"));
        assert!(key.contains("feishu"));
        assert!(key.starts_with("yang-system:"), "{key}");
    }

    #[test]
    fn different_deployments_get_different_keys() {
        assert_ne!(bucket_key("prod"), bucket_key("staging"));
    }

    #[test]
    fn interval_matches_the_rate() {
        // 整数除法向下取整：60000/90 = 666，60000/60 = 1000。
        assert_eq!(min_interval_ms(90), 666);
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
}
