//! Redis 授权版本缓存。

use anyhow::{ensure, Context};
use std::time::{Duration, Instant};
use yang_db::RedisClient;

pub(super) const CACHE_TTL_SECONDS: i64 = 5;
const MAX_DEPLOYMENT_LENGTH: usize = 64;
const SNAPSHOT_DEADLINE_SCRIPT: &str = r#"
local now = redis.call('TIME')
return now[1] * 1000 + math.floor(now[2] / 1000) + tonumber(ARGV[1])
"#;
const READ_SCRIPT: &str = r#"
return {redis.call('GET', KEYS[1]), redis.call('PTTL', KEYS[1])}
"#;
const PUBLISH_SCRIPT: &str = r#"
local now = redis.call('TIME')
if tonumber(ARGV[2]) <= now[1] * 1000 + math.floor(now[2] / 1000) then
    return 0
end
local current = redis.call('GET', KEYS[1])
local incoming = ARGV[1]

local function is_positive_decimal(value)
    return type(value) == 'string' and string.match(value, '^[1-9][0-9]*$') ~= nil
end

local should_update = not is_positive_decimal(current)
if not should_update then
    should_update = #incoming > #current or (#incoming == #current and incoming > current)
end

if should_update then
    redis.call('SET', KEYS[1], incoming, 'PXAT', ARGV[2])
    return 1
end
return 0
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePublishOutcome {
    Updated,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedAuthorizationVersion {
    Missing,
    Version(i64),
    Malformed,
}

/// 由事实读取前的 Redis 时间锚定，网络或线程暂停不能重新开启新鲜度窗口。
#[derive(Clone, Copy)]
pub(super) struct SnapshotDeadline(i64);

/// 只缓存事实快照；Outbox 事件只能失效缓存，不能重新建立历史版本。
#[derive(Clone)]
pub struct AuthorizationVersionCache {
    redis: RedisClient,
    deployment: String,
}

impl AuthorizationVersionCache {
    pub fn new(redis: RedisClient, deployment: impl Into<String>) -> anyhow::Result<Self> {
        let deployment = deployment.into();
        validate_deployment_name(&deployment)?;
        Ok(Self { redis, deployment })
    }

    /// 本部署的缓存命名空间。
    ///
    /// 暴露出来是给**其它 Redis 缓存**复用同一个部署标识——它们必须落在同一命名空间
    /// 下，否则「同一个部署」这个概念在缓存层会分裂成两份互不相干的键空间。
    /// 值本身已在构造期通过 [`validate_deployment_name`] 校验。
    pub(crate) fn deployment(&self) -> &str {
        &self.deployment
    }

    pub(super) async fn begin_snapshot(&self) -> anyhow::Result<SnapshotDeadline> {
        let deadline = self
            .redis
            .eval_script(
                &self.redis.script(SNAPSHOT_DEADLINE_SCRIPT),
                &[],
                &[(CACHE_TTL_SECONDS * 1000).to_string()],
            )
            .await
            .context("获取授权事实快照的 Redis 截止时间失败")?;
        Ok(SnapshotDeadline(deadline))
    }

    pub(super) async fn invalidate(&self, user_id: i64) -> anyhow::Result<CachePublishOutcome> {
        ensure!(user_id > 0, "授权缓存 user_id 必须是正整数");
        let deleted = self.redis.del(&[self.key(user_id)]).await?;
        Ok(if deleted > 0 {
            CachePublishOutcome::Updated
        } else {
            CachePublishOutcome::Ignored
        })
    }

    pub(super) async fn publish_snapshot(
        &self,
        user_id: i64,
        authz_version: i64,
        deadline: SnapshotDeadline,
    ) -> anyhow::Result<CachePublishOutcome> {
        ensure!(user_id > 0, "授权缓存 user_id 必须是正整数");
        ensure!(authz_version > 0, "授权缓存版本必须是正整数");
        let script = self.redis.script(PUBLISH_SCRIPT);
        let result: i64 = self
            .redis
            .eval_script(
                &script,
                &[self.key(user_id)],
                &[authz_version.to_string(), deadline.0.to_string()],
            )
            .await
            .context("单调发布 Redis 授权版本失败")?;
        match result {
            1 => Ok(CachePublishOutcome::Updated),
            0 => Ok(CachePublishOutcome::Ignored),
            value => anyhow::bail!("Redis 授权版本脚本返回未知结果: {value}"),
        }
    }

    /// 读取缓存中的授权版本，并把缺失与损坏值显式分流给事实库降级路径。
    pub async fn read(&self, user_id: i64) -> anyhow::Result<CachedAuthorizationVersion> {
        ensure!(user_id > 0, "授权缓存 user_id 必须是正整数");
        let started = Instant::now();
        let (value, remaining_ms): (Option<String>, i64) = self
            .redis
            .eval_script(&self.redis.script(READ_SCRIPT), &[self.key(user_id)], &[])
            .await
            .context("读取 Redis 授权版本失败")?;
        Ok(parse_cache_snapshot(value, remaining_ms, started.elapsed()))
    }

    fn key(&self, user_id: i64) -> String {
        format!("yang-system:{}:authz:version:{user_id}", self.deployment)
    }
}

fn parse_cache_snapshot(
    value: Option<String>,
    remaining_ms: i64,
    elapsed: Duration,
) -> CachedAuthorizationVersion {
    if remaining_ms <= 0 || elapsed >= Duration::from_millis(remaining_ms as u64) {
        return CachedAuthorizationVersion::Missing;
    }
    match value {
        None => CachedAuthorizationVersion::Missing,
        Some(value) => parse_cached_version(&value),
    }
}

fn parse_cached_version(value: &str) -> CachedAuthorizationVersion {
    match value.parse::<i64>() {
        Ok(version) if version > 0 && version.to_string() == value => {
            CachedAuthorizationVersion::Version(version)
        }
        _ => CachedAuthorizationVersion::Malformed,
    }
}

pub(crate) fn validate_deployment_name(deployment: &str) -> anyhow::Result<()> {
    ensure!(
        !deployment.is_empty() && deployment.len() <= MAX_DEPLOYMENT_LENGTH,
        "authorization.deployment 长度必须在 1..={MAX_DEPLOYMENT_LENGTH}"
    );
    ensure!(
        deployment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "authorization.deployment 只能包含小写 ASCII 字母、数字和连字符"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use yang_db::RedisConfig;

    #[test]
    fn deployment_is_a_bounded_stable_cache_namespace() {
        assert!(validate_deployment_name("prod-cn-1").is_ok());
        for deployment in ["", "UPPER", "contains_space", "slash/value"] {
            assert!(
                validate_deployment_name(deployment).is_err(),
                "部署命名空间必须拒绝: {deployment:?}"
            );
        }
    }

    #[test]
    fn cached_version_accepts_only_canonical_positive_i64() {
        assert_eq!(
            parse_cached_version("1"),
            CachedAuthorizationVersion::Version(1)
        );
        assert_eq!(
            parse_cached_version("9223372036854775807"),
            CachedAuthorizationVersion::Version(i64::MAX)
        );
        for malformed in [
            "",
            "0",
            "-1",
            "+1",
            "01",
            " 1",
            "1 ",
            "9223372036854775808",
            "malformed",
        ] {
            assert_eq!(
                parse_cached_version(malformed),
                CachedAuthorizationVersion::Malformed,
                "必须拒绝非规范缓存值: {malformed:?}"
            );
        }
    }

    #[test]
    fn cached_snapshot_requires_an_unexpired_budget_including_the_read() {
        for remaining in [-2, -1, 0, 1, 100] {
            assert_eq!(
                parse_cache_snapshot(Some("7".into()), remaining, Duration::from_millis(100)),
                CachedAuthorizationVersion::Missing,
            );
        }
        assert_eq!(
            parse_cache_snapshot(Some("7".into()), 101, Duration::from_millis(100)),
            CachedAuthorizationVersion::Version(7),
        );
    }

    #[tokio::test]
    #[ignore = "需要 YANG_SYSTEM_TEST_REDIS_URL 指向独立 Redis DB 15"]
    async fn real_redis_publish_is_monotonic_and_does_not_refresh_ignored_events(
    ) -> anyhow::Result<()> {
        let redis_url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")
            .context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
        ensure!(
            redis_url.trim_end_matches('/').ends_with("/15"),
            "授权缓存集成测试 Redis URL 必须使用独立 DB 15"
        );
        let redis = RedisClient::connect_with_config(
            &redis_url,
            RedisConfig::default()
                .with_max_connections(2)
                .with_min_connections(0)
                .with_connect_timeout(10),
        )
        .await?;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let cache = AuthorizationVersionCache::new(redis.clone(), format!("test-{suffix}"))?;
        let key = cache.key(42);
        redis.del(std::slice::from_ref(&key)).await?;
        ensure!(cache.read(42).await? == CachedAuthorizationVersion::Missing);

        ensure!(
            cache
                .publish_snapshot(42, 7, cache.begin_snapshot().await?)
                .await?
                == CachePublishOutcome::Updated,
            "首次发布必须写入缓存"
        );
        ensure!(redis.get(&key).await?.as_deref() == Some("7"));
        ensure!(cache.read(42).await? == CachedAuthorizationVersion::Version(7));
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        let decayed_ttl = redis.ttl(&key).await?;
        ensure!(
            (1..CACHE_TTL_SECONDS).contains(&decayed_ttl),
            "等待后 TTL 应已衰减: {decayed_ttl}"
        );

        for stale in [7, 6] {
            ensure!(
                cache
                    .publish_snapshot(42, stale, cache.begin_snapshot().await?)
                    .await?
                    == CachePublishOutcome::Ignored,
                "重复或旧事件必须被忽略: {stale}"
            );
            ensure!(redis.get(&key).await?.as_deref() == Some("7"));
            ensure!(
                redis.ttl(&key).await? <= decayed_ttl,
                "忽略事件不得延长 TTL"
            );
        }

        ensure!(
            cache
                .publish_snapshot(42, 8, cache.begin_snapshot().await?)
                .await?
                == CachePublishOutcome::Updated,
            "新版本必须更新缓存"
        );
        ensure!(redis.get(&key).await?.as_deref() == Some("8"));
        ensure!(
            redis.ttl(&key).await? >= CACHE_TTL_SECONDS - 1,
            "有效新版本应重新建立固定 TTL"
        );

        redis.setex(&key, CACHE_TTL_SECONDS, "malformed").await?;
        ensure!(cache.read(42).await? == CachedAuthorizationVersion::Malformed);
        ensure!(
            cache
                .publish_snapshot(42, i64::MAX, cache.begin_snapshot().await?)
                .await?
                == CachePublishOutcome::Updated,
            "可信事实快照必须修复非法缓存值"
        );
        ensure!(
            redis.get(&key).await?.as_deref() == Some("9223372036854775807"),
            "版本比较不得受 Lua 浮点精度限制"
        );
        ensure!(
            cache
                .publish_snapshot(42, i64::MAX - 1, cache.begin_snapshot().await?)
                .await?
                == CachePublishOutcome::Ignored,
            "接近 i64 上限的旧事件也不得回退缓存"
        );

        cache.invalidate(42).await?;
        let deadline = cache.begin_snapshot().await?;
        let short_deadline = SnapshotDeadline(deadline.0 - 4_700);
        ensure!(
            cache.publish_snapshot(42, 9, short_deadline).await? == CachePublishOutcome::Updated
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
        ensure!(
            cache.read(42).await? == CachedAuthorizationVersion::Missing,
            "回填必须使用读取前锚定的绝对期限，不能从发布时重新计时"
        );
        ensure!(
            cache.publish_snapshot(42, 9, short_deadline).await? == CachePublishOutcome::Ignored,
            "迟到的事实快照不能在截止后重新创建缓存"
        );
        cache.invalidate(42).await?;
        ensure!(cache.read(42).await? == CachedAuthorizationVersion::Missing);

        redis.del(&[key]).await?;
        redis.close().await;
        Ok(())
    }
}
