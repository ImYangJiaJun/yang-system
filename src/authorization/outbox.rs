//! 授权事务 Outbox 的 MySQL 状态机。
//! raw-sql-boundary: infrastructure-repository authorization-outbox

use crate::config::AuthorizationSettings;
use anyhow::{ensure, Context};
use sqlx::MySqlPool;
use std::collections::BTreeMap;

const MAX_LAST_ERROR_CHARS: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaimedAuthorizationEvent {
    pub(super) id: i64,
    pub(super) user_id: i64,
    pub(super) authz_version: i64,
    pub(super) attempts: u32,
    pub(super) created_at: i64,
}

#[derive(Clone)]
pub(super) struct AuthorizationOutboxRepository {
    pool: MySqlPool,
}

impl AuthorizationOutboxRepository {
    pub(super) fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    pub(super) async fn validate_schema(&self) -> anyhow::Result<()> {
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT COLUMN_NAME FROM information_schema.columns \
             WHERE table_schema = DATABASE() AND table_name = 'authorization_outbox' \
             ORDER BY ORDINAL_POSITION",
        )
        .fetch_all(&self.pool)
        .await
        .context("读取授权 Outbox 列失败")?;
        let expected_columns = [
            "id",
            "user_id",
            "authz_version",
            "state",
            "attempts",
            "available_at",
            "lease_until",
            "worker_id",
            "created_at",
            "published_at",
            "last_error",
        ];
        ensure!(
            columns.iter().map(String::as_str).eq(expected_columns),
            "authorization_outbox 列未对齐，请先执行版本化迁移: {columns:?}"
        );

        let index_rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT INDEX_NAME, NON_UNIQUE, \
                    GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR ',') \
             FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'authorization_outbox' \
             GROUP BY INDEX_NAME, NON_UNIQUE",
        )
        .fetch_all(&self.pool)
        .await
        .context("读取授权 Outbox 索引失败")?;
        let indexes = index_rows
            .into_iter()
            .map(|(name, non_unique, columns)| (name, (non_unique, columns)))
            .collect::<BTreeMap<_, _>>();
        for (name, non_unique, columns) in [
            ("PRIMARY", 0, "id"),
            (
                "uk_authorization_outbox_user_version",
                0,
                "user_id,authz_version",
            ),
            (
                "idx_authorization_outbox_dispatch",
                1,
                "state,available_at,id",
            ),
            (
                "idx_authorization_outbox_user_version",
                1,
                "user_id,authz_version",
            ),
        ] {
            ensure!(
                indexes.get(name) == Some(&(non_unique, columns.to_string())),
                "authorization_outbox 索引 {name} 未对齐，请先执行版本化迁移: {indexes:?}"
            );
        }
        Ok(())
    }

    pub(super) async fn claim(
        &self,
        settings: &AuthorizationSettings,
        worker_id: &str,
    ) -> anyhow::Result<Vec<ClaimedAuthorizationEvent>> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("开启 Outbox claim 事务失败")?;
        // MySQL 会缓存预编译语句的索引范围。若把 UNIX_TIMESTAMP() 直接放进范围谓词，
        // 首次空轮询时的时间边界可能被后续执行复用，导致后来写入的事件永久不可见。
        // 先在同一事务内采样数据库时钟，再把边界作为参数绑定，既保持索引可用，
        // 又确保每轮 claim 使用新的、跨实例一致的时间。
        let now: i64 = sqlx::query_scalar("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)")
            .fetch_one(&mut *transaction)
            .await
            .context("读取 Outbox claim 数据库时钟失败")?;
        ensure!(now > 0, "Outbox claim 数据库时钟无效");
        let rows: Vec<(i64, i64, i64, u32, i64)> = sqlx::query_as(
            "SELECT id, user_id, authz_version, attempts, created_at \
             FROM authorization_outbox \
             WHERE (state = 'pending' AND available_at <= ?) \
                OR (state = 'processing' AND lease_until <= ?) \
             ORDER BY id \
             LIMIT ? FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .bind(now)
        .bind(settings.outbox_batch_size)
        .fetch_all(&mut *transaction)
        .await
        .context("锁定待发布授权 Outbox 事件失败")?;

        let mut events = Vec::with_capacity(rows.len());
        for (id, user_id, authz_version, attempts, created_at) in rows {
            ensure!(
                id > 0 && user_id > 0 && authz_version > 0,
                "授权 Outbox 事件字段无效"
            );
            let attempts = attempts
                .checked_add(1)
                .context("授权 Outbox 事件重试次数已耗尽")?;
            let result = sqlx::query(
                "UPDATE authorization_outbox \
                 SET state = 'processing', attempts = ?, \
                     lease_until = ? + ?, worker_id = ?, last_error = NULL \
                 WHERE id = ?",
            )
            .bind(attempts)
            .bind(now)
            .bind(settings.outbox_lease_seconds)
            .bind(worker_id)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("claim 授权 Outbox 事件失败: id={id}"))?;
            ensure!(
                result.rows_affected() == 1,
                "claim 授权 Outbox 事件影响行数异常: id={id}"
            );
            events.push(ClaimedAuthorizationEvent {
                id,
                user_id,
                authz_version,
                attempts,
                created_at,
            });
        }
        transaction
            .commit()
            .await
            .context("提交 Outbox claim 事务失败")?;
        Ok(events)
    }

    pub(super) async fn mark_published(
        &self,
        event: &ClaimedAuthorizationEvent,
        worker_id: &str,
    ) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE authorization_outbox \
             SET state = 'published', published_at = UNIX_TIMESTAMP(), \
                 lease_until = NULL, worker_id = NULL, last_error = NULL \
             WHERE id = ? AND state = 'processing' AND worker_id = ?",
        )
        .bind(event.id)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("确认授权 Outbox 发布完成失败: id={}", event.id))?;
        ensure!(
            result.rows_affected() == 1,
            "确认授权 Outbox 发布完成时租约已丢失: id={}",
            event.id
        );
        Ok(())
    }

    pub(super) async fn schedule_retry(
        &self,
        event: &ClaimedAuthorizationEvent,
        worker_id: &str,
        max_retry_seconds: u64,
        error: &anyhow::Error,
    ) -> anyhow::Result<u64> {
        let delay_seconds = retry_delay_seconds(event.attempts, max_retry_seconds);
        let last_error = bounded_error(error);
        let result = sqlx::query(
            "UPDATE authorization_outbox \
             SET state = 'pending', available_at = UNIX_TIMESTAMP() + ?, \
                 lease_until = NULL, worker_id = NULL, last_error = ? \
             WHERE id = ? AND state = 'processing' AND worker_id = ?",
        )
        .bind(delay_seconds)
        .bind(last_error)
        .bind(event.id)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("安排授权 Outbox 重试失败: id={}", event.id))?;
        ensure!(
            result.rows_affected() == 1,
            "安排授权 Outbox 重试时租约已丢失: id={}",
            event.id
        );
        Ok(delay_seconds)
    }

    pub(super) async fn backlog(&self) -> anyhow::Result<(u64, u64)> {
        let (pending, oldest_age): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), \
                    COALESCE(MAX(GREATEST(UNIX_TIMESTAMP() - created_at, 0)), 0) \
             FROM authorization_outbox WHERE state <> 'published'",
        )
        .fetch_one(&self.pool)
        .await
        .context("读取授权 Outbox 积压指标失败")?;
        ensure!(
            pending >= 0 && oldest_age >= 0,
            "授权 Outbox 积压指标不得为负数"
        );
        Ok((pending as u64, oldest_age as u64))
    }
}

fn retry_delay_seconds(attempts: u32, max_retry_seconds: u64) -> u64 {
    let exponent = attempts.saturating_sub(1).min(62);
    1_u64
        .checked_shl(exponent)
        .unwrap_or(u64::MAX)
        .min(max_retry_seconds)
}

fn bounded_error(error: &anyhow::Error) -> String {
    format!("{error:#}")
        .chars()
        .take(MAX_LAST_ERROR_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use yang_db::{Database, DatabaseConfig};

    #[test]
    fn retry_is_exponential_bounded_and_overflow_safe() {
        assert_eq!(retry_delay_seconds(1, 60), 1);
        assert_eq!(retry_delay_seconds(2, 60), 2);
        assert_eq!(retry_delay_seconds(6, 60), 32);
        assert_eq!(retry_delay_seconds(7, 60), 60);
        assert_eq!(retry_delay_seconds(u32::MAX, 60), 60);
    }

    #[test]
    fn persisted_error_is_character_bounded() {
        let error = anyhow::anyhow!("错".repeat(MAX_LAST_ERROR_CHARS + 10));
        let bounded = bounded_error(&error);
        assert_eq!(bounded.chars().count(), MAX_LAST_ERROR_CHARS);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[tokio::test]
    #[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL"]
    async fn claim_refreshes_prepared_statement_time_boundary() -> anyhow::Result<()> {
        let mysql_url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
            .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = Database::connect_with_config(
            &mysql_url,
            DatabaseConfig::default()
                .with_max_connections(1)
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

        let repository = AuthorizationOutboxRepository::new(database.pool().clone());
        let settings = AuthorizationSettings {
            deployment: "outbox-boundary-regression".to_string(),
            outbox_poll_interval_ms: 10,
            outbox_batch_size: 10,
            outbox_lease_seconds: 5,
            outbox_max_retry_seconds: 5,
        };
        assert!(
            repository.claim(&settings, "cold-poll").await?.is_empty(),
            "首次空轮询必须返回空批次"
        );
        sqlx::query(
            "INSERT INTO authorization_outbox \
             (user_id, authz_version, available_at, created_at) \
             VALUES (900001, 2, UNIX_TIMESTAMP() + 1, UNIX_TIMESTAMP())",
        )
        .execute(database.pool())
        .await?;
        tokio::time::sleep(Duration::from_millis(2_100)).await;

        let claimed = repository.claim(&settings, "warm-poll").await?;
        assert_eq!(
            claimed.len(),
            1,
            "空轮询后写入的到期事件不得受预编译时间边界影响"
        );
        assert_eq!(claimed[0].user_id, 900001);

        sqlx::query("DROP TABLE IF EXISTS authorization_outbox")
            .execute(database.pool())
            .await?;
        database.close().await;
        Ok(())
    }
}
