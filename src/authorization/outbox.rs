//! 授权事务 Outbox 的 MySQL 状态机。

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
        let rows: Vec<(i64, i64, i64, u32, i64)> = sqlx::query_as(
            "SELECT id, user_id, authz_version, attempts, created_at \
             FROM authorization_outbox \
             WHERE (state = 'pending' AND available_at <= UNIX_TIMESTAMP()) \
                OR (state = 'processing' AND lease_until <= UNIX_TIMESTAMP()) \
             ORDER BY id \
             LIMIT ? FOR UPDATE SKIP LOCKED",
        )
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
                     lease_until = UNIX_TIMESTAMP() + ?, worker_id = ?, last_error = NULL \
                 WHERE id = ?",
            )
            .bind(attempts)
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
}
