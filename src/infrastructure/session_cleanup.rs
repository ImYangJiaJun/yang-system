//! `user_session` 行的保留期清理 Worker。
//!
//! 会话行在登录时插入、refresh 时节流更新 `last_seen_at`；撤销只打标记不删除。
//! 若不做保留期清理，IP/UA 与每台设备的会话记录会无界累积。
//!
//! 保留期 = refresh token 硬上限 + 安全余量。`last_seen_at` 有 60 秒节流，
//! 且 touch 失败只记日志不回滚 refresh；应用时钟与数据库时钟也可能有偏差。
//! 余量覆盖这些窗口，确保清理不会删掉仍持有有效 refresh token 的会话。

use anyhow::Context;
use sqlx::MySqlPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use yang_base::tools::Tools;
use yang_db::{field, table, CompareOp, QueryBuilder, SqlExpr};

/// 安全余量：覆盖 `last_seen_at` 的 60 秒节流、touch 失败窗口与时钟漂移。
const SESSION_RETENTION_MARGIN_SECONDS: u64 = 7 * 24 * 60 * 60;

/// 会话行保留期（秒）：refresh token 硬上限 + 余量。
const SESSION_RETENTION_SECONDS: u64 =
    crate::config::MAX_REFRESH_TTL_SECONDS + SESSION_RETENTION_MARGIN_SECONDS;

/// 清理扫描间隔：保留期以天计，6 小时一轮足以跟上增长。
const SESSION_CLEANUP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// 与 HTTP 服务同生共死的会话清理 Worker 句柄。
pub struct SessionCleanupWorker {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl SessionCleanupWorker {
    pub fn start(tools: Arc<Tools>) -> anyhow::Result<Self> {
        let pool = tools.mysql()?.pool().clone();
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_loop(pool, receiver));
        Ok(Self {
            shutdown,
            task: Some(task),
        })
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await.context("等待会话清理 Worker 退出失败")?;
        Ok(())
    }
}

impl Drop for SessionCleanupWorker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn run_loop(pool: MySqlPool, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        match cleanup_once(&pool).await {
            Ok(deleted) => {
                metrics::counter!("session_cleanup_runs_total", "result" => "success").increment(1);
                metrics::counter!("session_cleanup_deleted_total").increment(deleted);
                tracing::info!(deleted, "会话保留期清理完成");
            }
            Err(error) => {
                metrics::counter!("session_cleanup_runs_total", "result" => "error").increment(1);
                tracing::error!(error = %error, "会话保留期清理失败");
            }
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            () = tokio::time::sleep(SESSION_CLEANUP_INTERVAL) => {}
        }
    }
}

async fn cleanup_once(pool: &MySqlPool) -> anyhow::Result<u64> {
    // 用数据库时钟做比较（DSL 的 UNIX_TIMESTAMP() 表达式），不把应用时钟带进
    // 删除谓词；同时避免在 production 代码里出现裸 SQL。
    let deleted = QueryBuilder::from_pool(pool, table!("user_session"))
        .where_expr(
            field!("last_seen_at"),
            CompareOp::Lt,
            SqlExpr::unix_timestamp_add(-(SESSION_RETENTION_SECONDS as i64)),
        )?
        .delete()
        .await
        .context("删除过期 user_session 行失败")?;
    Ok(deleted)
}

/// 删除 `last_seen_at` 早于保留期的会话行，返回删除行数。
#[cfg(test)]
pub(crate) async fn delete_stale_sessions(pool: &MySqlPool) -> anyhow::Result<u64> {
    cleanup_once(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::ensure;
    use yang_base::database::DatabaseInitializer;
    use yang_db::{Database, DatabaseConfig};

    #[test]
    fn retention_covers_refresh_token_hard_limit_with_margin() {
        let retention = SESSION_RETENTION_SECONDS;
        let hard_limit = crate::config::MAX_REFRESH_TTL_SECONDS;
        assert!(
            retention > hard_limit,
            "会话保留期必须严格长于 refresh token 硬上限"
        );
        assert!(
            retention - hard_limit >= 24 * 3600,
            "余量至少覆盖 last_seen 节流窗口与时钟漂移"
        );
    }

    #[test]
    fn cleanup_interval_is_positive() {
        assert!(!SESSION_CLEANUP_INTERVAL.is_zero());
    }

    #[tokio::test]
    #[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL"]
    async fn real_mysql_deletes_only_sessions_older_than_retention() -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
            .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = Database::connect_with_config(
            &url,
            DatabaseConfig::default()
                .with_max_connections(4)
                .with_min_connections(0)
                .with_connect_timeout(10),
        )
        .await?;
        let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        ensure!(
            name.as_deref().is_some_and(|name| name.ends_with("_test")),
            "会话清理集成测试只能使用 _test 数据库"
        );

        sqlx::query("DROP TABLE IF EXISTS user_session")
            .execute(database.pool())
            .await?;
        let definition = crate::infrastructure::schema::user_session()?;
        let initializer_database = Database::from_pool(
            database.pool().clone(),
            DatabaseConfig::default().with_max_connections(4),
        )?;
        DatabaseInitializer::new(initializer_database)
            .sync_table_definitions(&[&definition])
            .await?;

        let now: i64 = sqlx::query_scalar("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)")
            .fetch_one(database.pool())
            .await?;
        let day = 24 * 3600;
        let retention = SESSION_RETENTION_SECONDS as i64;
        for (session_id, last_seen_at) in [
            ("stale-session", now - retention - 1),
            ("boundary-session", now - retention),
            ("fresh-session", now - day),
        ] {
            sqlx::query(
                "INSERT INTO user_session \
                 (session_id, user_id, current_jti, created_at, last_seen_at, ip, user_agent) \
                 VALUES (?, 1, ?, ?, ?, '127.0.0.1', 'session-cleanup-test')",
            )
            .bind(session_id)
            .bind(format!("jti-{session_id}"))
            .bind(last_seen_at)
            .bind(last_seen_at)
            .execute(database.pool())
            .await?;
        }

        let deleted = delete_stale_sessions(database.pool()).await?;
        ensure!(deleted == 1, "只应删除超过保留期的会话，实际删除 {deleted}");
        let remaining: Vec<String> =
            sqlx::query_scalar("SELECT session_id FROM user_session ORDER BY session_id")
                .fetch_all(database.pool())
                .await?;
        ensure!(
            remaining == vec!["boundary-session".to_string(), "fresh-session".to_string()],
            "恰好 90 天与更近的会话必须保留: {remaining:?}"
        );

        sqlx::query("DROP TABLE IF EXISTS user_session")
            .execute(database.pool())
            .await?;
        database.close().await;
        Ok(())
    }
}
