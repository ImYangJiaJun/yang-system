//! 授权 Outbox 至 Redis 的可靠传播 Worker。

use super::outbox::{AuthorizationOutboxRepository, ClaimedAuthorizationEvent};
use super::{AuthorizationVersionCache, CachePublishOutcome};
use crate::config::AuthorizationSettings;
use anyhow::{ensure, Context};
use async_trait::async_trait;
use sqlx::MySqlPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use yang_base::tools::Tools;

static WORKER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuthorizationOutboxBatchReport {
    pub claimed: u64,
    pub published: u64,
    pub retried: u64,
}

#[async_trait]
trait AuthorizationVersionPublisher: Send + Sync {
    async fn publish(
        &self,
        user_id: i64,
        authz_version: i64,
    ) -> anyhow::Result<CachePublishOutcome>;
}

#[async_trait]
impl AuthorizationVersionPublisher for AuthorizationVersionCache {
    async fn publish(
        &self,
        user_id: i64,
        authz_version: i64,
    ) -> anyhow::Result<CachePublishOutcome> {
        AuthorizationVersionCache::publish(self, user_id, authz_version).await
    }
}

struct AuthorizationOutboxProcessor {
    repository: AuthorizationOutboxRepository,
    publisher: Arc<dyn AuthorizationVersionPublisher>,
    settings: AuthorizationSettings,
    worker_id: String,
}

impl AuthorizationOutboxProcessor {
    fn new(
        pool: MySqlPool,
        publisher: Arc<dyn AuthorizationVersionPublisher>,
        settings: AuthorizationSettings,
        worker_id: String,
    ) -> Self {
        Self {
            repository: AuthorizationOutboxRepository::new(pool),
            publisher,
            settings,
            worker_id,
        }
    }

    async fn process_once(&self) -> anyhow::Result<AuthorizationOutboxBatchReport> {
        let events = self
            .repository
            .claim(&self.settings, &self.worker_id)
            .await?;
        let mut report = AuthorizationOutboxBatchReport {
            claimed: events.len() as u64,
            ..AuthorizationOutboxBatchReport::default()
        };
        for event in events {
            match self
                .publisher
                .publish(event.user_id, event.authz_version)
                .await
            {
                Ok(outcome) => {
                    self.repository
                        .mark_published(&event, &self.worker_id)
                        .await?;
                    report.published += 1;
                    record_publish_success(&event, outcome);
                }
                Err(error) => {
                    let delay = self
                        .repository
                        .schedule_retry(
                            &event,
                            &self.worker_id,
                            self.settings.outbox_max_retry_seconds,
                            &error,
                        )
                        .await?;
                    report.retried += 1;
                    metrics::counter!("authz_outbox_publish_total", "result" => "retry")
                        .increment(1);
                    tracing::warn!(
                        event_id = event.id,
                        user_id = event.user_id,
                        authz_version = event.authz_version,
                        attempts = event.attempts,
                        retry_delay_seconds = delay,
                        error = %error,
                        "授权 Outbox 发布失败，已安排重试"
                    );
                }
            }
        }
        self.refresh_backlog_metrics().await?;
        Ok(report)
    }

    async fn refresh_backlog_metrics(&self) -> anyhow::Result<()> {
        let (pending, oldest_age) = self.repository.backlog().await?;
        metrics::gauge!("authz_outbox_pending").set(pending as f64);
        metrics::gauge!("authz_outbox_oldest_age_seconds").set(oldest_age as f64);
        tracing::debug!(
            authz_outbox_pending = pending,
            authz_outbox_oldest_age_seconds = oldest_age,
            "授权 Outbox 积压快照"
        );
        Ok(())
    }
}

/// 与 HTTP 服务同生共死的授权传播 Worker 句柄。
pub struct AuthorizationOutboxWorker {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl AuthorizationOutboxWorker {
    pub async fn start(tools: &Tools, settings: AuthorizationSettings) -> anyhow::Result<Self> {
        let repository = AuthorizationOutboxRepository::new(tools.mysql()?.pool().clone());
        repository.validate_schema().await?;
        let cache = tools.extension::<AuthorizationVersionCache>()?.clone();
        let processor = AuthorizationOutboxProcessor::new(
            tools.mysql()?.pool().clone(),
            Arc::new(cache),
            settings.clone(),
            new_worker_id()?,
        );
        let poll_interval = Duration::from_millis(settings.outbox_poll_interval_ms);
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_loop(processor, poll_interval, receiver));
        Ok(Self { shutdown, task })
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        self.task.await.context("等待授权 Outbox Worker 退出失败")?;
        Ok(())
    }
}

async fn run_loop(
    processor: AuthorizationOutboxProcessor,
    poll_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    tracing::info!(worker_id = %processor.worker_id, "授权 Outbox Worker 已启动");
    loop {
        if *shutdown.borrow() {
            break;
        }
        match processor.process_once().await {
            Ok(report) if report.claimed > 0 => {
                tracing::info!(
                    worker_id = %processor.worker_id,
                    claimed = report.claimed,
                    published = report.published,
                    retried = report.retried,
                    "授权 Outbox 批次处理完成"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(
                    worker_id = %processor.worker_id,
                    error = %error,
                    "授权 Outbox 批次处理失败"
                );
            }
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            () = tokio::time::sleep(poll_interval) => {}
        }
    }
    tracing::info!(worker_id = %processor.worker_id, "授权 Outbox Worker 已停止");
}

fn record_publish_success(event: &ClaimedAuthorizationEvent, outcome: CachePublishOutcome) {
    metrics::counter!("authz_outbox_publish_total", "result" => "success").increment(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let propagation_seconds = now.saturating_sub(event.created_at.max(0) as u64);
    metrics::histogram!("authz_propagation_seconds").record(propagation_seconds as f64);
    tracing::debug!(
        event_id = event.id,
        user_id = event.user_id,
        authz_version = event.authz_version,
        attempts = event.attempts,
        cache_outcome = ?outcome,
        authz_propagation_seconds = propagation_seconds,
        "授权版本已传播至 Redis"
    );
}

fn new_worker_id() -> anyhow::Result<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix epoch")?
        .as_millis();
    let sequence = WORKER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let worker_id = format!("{}-{timestamp}-{sequence}", std::process::id());
    ensure!(worker_id.len() <= 128, "授权 Outbox worker_id 超长");
    Ok(worker_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::outbox::AuthorizationOutboxRepository;
    use std::sync::atomic::AtomicUsize;
    use yang_db::{Database, DatabaseConfig, RedisClient, RedisConfig};

    struct FailingPublisher {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AuthorizationVersionPublisher for FailingPublisher {
        async fn publish(
            &self,
            _user_id: i64,
            _authz_version: i64,
        ) -> anyhow::Result<CachePublishOutcome> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("forced publisher failure")
        }
    }

    fn settings(deployment: String) -> AuthorizationSettings {
        AuthorizationSettings {
            deployment,
            outbox_poll_interval_ms: 10,
            outbox_batch_size: 100,
            outbox_lease_seconds: 2,
            outbox_max_retry_seconds: 60,
        }
    }

    #[tokio::test]
    #[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
    async fn real_outbox_supports_concurrent_claim_retry_and_expired_lease_replay(
    ) -> anyhow::Result<()> {
        let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
            .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
        let redis_url = std::env::var("YANG_SYSTEM_TEST_REDIS_URL")
            .context("缺少 YANG_SYSTEM_TEST_REDIS_URL")?;
        ensure!(
            redis_url.trim_end_matches('/').ends_with("/15"),
            "授权 Outbox 集成测试 Redis URL 必须使用独立 DB 15"
        );
        let database = Database::connect_with_config(
            &mysql_url,
            DatabaseConfig::default()
                .with_max_connections(8)
                .with_min_connections(0)
                .with_connect_timeout(10),
        )
        .await?;
        let database_name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        ensure!(
            database_name.is_some_and(|name| name.ends_with("_test")),
            "授权 Outbox 集成测试只能使用 _test 数据库"
        );
        sqlx::query("DROP TABLE IF EXISTS authorization_outbox")
            .execute(database.pool())
            .await?;
        sqlx::raw_sql(include_str!(
            "../../migrations/20260726_0006_create_authorization_outbox.sql"
        ))
        .execute(database.pool())
        .await?;
        let redis = RedisClient::connect_with_config(
            &redis_url,
            RedisConfig::default()
                .with_max_connections(4)
                .with_min_connections(0)
                .with_connect_timeout(10),
        )
        .await?;
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let deployment = format!("worker-test-{suffix}");
        let worker_settings = settings(deployment.clone());
        let publisher = Arc::new(AuthorizationVersionCache::new(
            redis.clone(),
            deployment.clone(),
        )?);

        for (user_id, authz_version) in (1_i64..=20).map(|offset| (700_000 + offset, 2)) {
            sqlx::query(
                "INSERT INTO authorization_outbox \
                 (user_id, authz_version, available_at, created_at) \
                 VALUES (?, ?, UNIX_TIMESTAMP(), UNIX_TIMESTAMP())",
            )
            .bind(user_id)
            .bind(authz_version)
            .execute(database.pool())
            .await?;
        }
        let first = AuthorizationOutboxProcessor::new(
            database.pool().clone(),
            publisher.clone(),
            worker_settings.clone(),
            "concurrent-a".to_string(),
        );
        let second = AuthorizationOutboxProcessor::new(
            database.pool().clone(),
            publisher.clone(),
            worker_settings.clone(),
            "concurrent-b".to_string(),
        );
        let (first_report, second_report) =
            tokio::join!(first.process_once(), second.process_once());
        ensure!(
            first_report?.published + second_report?.published == 20,
            "并发 Worker 必须恰好发布全部事件"
        );
        let published: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM authorization_outbox WHERE state = 'published'",
        )
        .fetch_one(database.pool())
        .await?;
        ensure!(published == 20, "并发 claim 不得重复或遗漏事件");

        sqlx::query(
            "INSERT INTO authorization_outbox \
             (user_id, authz_version, state, attempts, available_at, lease_until, worker_id, created_at) \
             VALUES (800001, 9, 'processing', 1, UNIX_TIMESTAMP(), UNIX_TIMESTAMP() - 1, 'dead-worker', UNIX_TIMESTAMP())",
        )
        .execute(database.pool())
        .await?;
        let replay = AuthorizationOutboxProcessor::new(
            database.pool().clone(),
            publisher.clone(),
            worker_settings.clone(),
            "replay-worker".to_string(),
        )
        .process_once()
        .await?;
        ensure!(replay.published == 1, "过期租约必须被新 Worker 重放");

        sqlx::query(
            "INSERT INTO authorization_outbox \
             (user_id, authz_version, available_at, created_at) \
             VALUES (800002, 3, UNIX_TIMESTAMP(), UNIX_TIMESTAMP())",
        )
        .execute(database.pool())
        .await?;
        let failing = Arc::new(FailingPublisher {
            calls: AtomicUsize::new(0),
        });
        let retry = AuthorizationOutboxProcessor::new(
            database.pool().clone(),
            failing.clone(),
            worker_settings.clone(),
            "retry-worker".to_string(),
        )
        .process_once()
        .await?;
        ensure!(retry.retried == 1, "发布失败必须安排重试");
        ensure!(failing.calls.load(Ordering::Relaxed) == 1);
        let retry_row: (String, u32, Option<String>) = sqlx::query_as(
            "SELECT state, attempts, last_error FROM authorization_outbox \
             WHERE user_id = 800002 AND authz_version = 3",
        )
        .fetch_one(database.pool())
        .await?;
        ensure!(
            retry_row.0 == "pending"
                && retry_row.1 == 1
                && retry_row
                    .2
                    .as_ref()
                    .is_some_and(|error| error.contains("forced publisher failure")),
            "发布失败必须释放租约并保存有界错误: {retry_row:?}"
        );

        let repository = AuthorizationOutboxRepository::new(database.pool().clone());
        repository.validate_schema().await?;
        sqlx::query(
            "UPDATE authorization_outbox SET available_at = UNIX_TIMESTAMP() \
             WHERE user_id = 800002 AND authz_version = 3",
        )
        .execute(database.pool())
        .await?;
        let claimed = repository
            .claim(&worker_settings, "crashed-after-redis")
            .await?;
        ensure!(claimed.len() == 1, "重试事件应可再次 claim");
        publisher
            .publish(claimed[0].user_id, claimed[0].authz_version)
            .await?;
        sqlx::query(
            "UPDATE authorization_outbox SET lease_until = UNIX_TIMESTAMP() - 1 \
             WHERE id = ?",
        )
        .bind(claimed[0].id)
        .execute(database.pool())
        .await?;
        let replay_after_mark_failure = AuthorizationOutboxProcessor::new(
            database.pool().clone(),
            publisher,
            worker_settings,
            "mark-replay-worker".to_string(),
        )
        .process_once()
        .await?;
        ensure!(
            replay_after_mark_failure.published == 1,
            "Redis 成功但 DB 未确认的事件必须依赖单调幂等发布完成重放"
        );

        let keys = (1_i64..=20)
            .map(|offset| {
                format!(
                    "yang-system:{deployment}:authz:version:{}",
                    700_000 + offset
                )
            })
            .chain([
                format!("yang-system:{deployment}:authz:version:800001"),
                format!("yang-system:{deployment}:authz:version:800002"),
            ])
            .collect::<Vec<_>>();
        redis.del(&keys).await?;
        sqlx::query("DROP TABLE IF EXISTS authorization_outbox")
            .execute(database.pool())
            .await?;
        redis.close().await;
        database.close().await;
        Ok(())
    }
}
