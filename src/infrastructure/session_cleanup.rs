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
use yang_base::tools::Tools;
use yang_db::{field, table, CompareOp, QueryBuilder, SqlExpr};
use yang_runtime::shutdown::OwnedTask;

/// 安全余量：覆盖 `last_seen_at` 的 60 秒节流、touch 失败窗口与时钟漂移。
const SESSION_RETENTION_MARGIN_SECONDS: u64 = 7 * 24 * 60 * 60;

/// 会话行保留期（秒）：refresh token 硬上限 + 余量。
const SESSION_RETENTION_SECONDS: u64 =
    crate::config::MAX_REFRESH_TTL_SECONDS + SESSION_RETENTION_MARGIN_SECONDS;

/// 清理扫描间隔：保留期以天计，6 小时一轮足以跟上增长。
const SESSION_CLEANUP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// 单批最多删除 1000 行；积压批次之间让出执行权并检查关闭信号。
const SESSION_CLEANUP_BATCH_SIZE: u64 = 1000;
const SESSION_CLEANUP_BACKLOG_DELAY: Duration = Duration::from_millis(100);

/// 与 HTTP 服务同生共死的会话清理 Worker 句柄。
pub struct SessionCleanupWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
}

impl SessionCleanupWorker {
    pub fn start(tools: Arc<Tools>) -> anyhow::Result<Self> {
        let pool = tools.mysql()?.pool().clone();
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_loop(pool, receiver));
        Ok(Self {
            shutdown,
            task: OwnedTask::new(task),
        })
    }

    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        self.task
            .join()
            .await
            .context("等待会话清理 Worker 退出失败")?;
        Ok(())
    }
}

impl Drop for SessionCleanupWorker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn run_loop(pool: MySqlPool, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        let mut delay = SESSION_CLEANUP_INTERVAL;
        match cleanup_once(&pool).await {
            Ok(deleted) => {
                metrics::counter!("session_cleanup_runs_total", "result" => "success").increment(1);
                metrics::counter!("session_cleanup_deleted_total").increment(deleted);
                tracing::info!(deleted, "会话保留期清理完成");
                if deleted >= SESSION_CLEANUP_BATCH_SIZE {
                    delay = SESSION_CLEANUP_BACKLOG_DELAY;
                }
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
            () = tokio::time::sleep(delay) => {}
        }
    }
}

async fn cleanup_once(pool: &MySqlPool) -> anyhow::Result<u64> {
    // QueryBuilder 的 limit 只约束 SELECT，不能对 delete() 链接 limit 冒充有界删除。
    let candidates: Vec<(String,)> = QueryBuilder::from_pool(pool, table!("user_session"))
        .field(field!("session_id"))
        .where_expr(
            field!("last_seen_at"),
            CompareOp::Lt,
            SqlExpr::unix_timestamp_add(-(SESSION_RETENTION_SECONDS as i64)),
        )?
        .limit(SESSION_CLEANUP_BATCH_SIZE)
        .select()
        .await
        .context("查询过期会话批次失败")?;
    delete_stale_candidates(pool, candidates.into_iter().map(|(id,)| id).collect()).await
}

async fn delete_stale_candidates(pool: &MySqlPool, candidates: Vec<String>) -> anyhow::Result<u64> {
    // 用数据库时钟做比较（DSL 的 UNIX_TIMESTAMP() 表达式），不把应用时钟带进
    // 删除谓词；重新检查保留期，保护 SELECT 后被 refresh 触碰的会话。
    let deleted = QueryBuilder::from_pool(pool, table!("user_session"))
        .where_in(field!("session_id"), candidates)
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

    #[tokio::test]
    async fn cancelled_shutdown_aborts_inflight_cleanup() -> anyhow::Result<()> {
        let (shutdown, _receiver) = watch::channel(false);
        let (started, ready) = tokio::sync::oneshot::channel();
        let (dropped, released) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _resource = dropped;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await?;
        let worker = SessionCleanupWorker {
            shutdown,
            task: OwnedTask::new(task),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), worker.shutdown())
                .await
                .is_err()
        );
        // 关闭 future 被总预算取消后，仍必须销毁正在运行的任务所持资源。
        assert!(
            tokio::time::timeout(Duration::from_secs(1), released)
                .await
                .is_ok(),
            "关闭等待被取消后后台任务仍在运行"
        );
        Ok(())
    }

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
            "保留期边界与更近日期的会话必须保留: {remaining:?}"
        );

        sqlx::query("DROP TABLE IF EXISTS user_session")
            .execute(database.pool())
            .await?;
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL"]
    async fn real_mysql_cleanup_batches_and_drains_backlog() -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        ensure!(name.ends_with("_test"), "拒绝在非测试数据库清理会话");
        sqlx::query("DROP TABLE IF EXISTS user_session")
            .execute(database.pool())
            .await?;
        let definition = crate::infrastructure::schema::user_session()?;
        let initializer = Database::from_pool(database.pool().clone(), DatabaseConfig::default())?;
        DatabaseInitializer::new(initializer)
            .sync_table_definitions(&[&definition])
            .await?;
        let now: i64 = sqlx::query_scalar("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)")
            .fetch_one(database.pool())
            .await?;
        let stale = now - SESSION_RETENTION_SECONDS as i64 - 3600;
        // 超过单批上限，显式调用只应删除 1000 行。
        insert_stale_batch(database.pool(), stale, 1001).await?;
        ensure!(
            cleanup_once(database.pool()).await? == 1000,
            "单轮清理必须有行数上限"
        );
        ensure!(
            cleanup_once(database.pool()).await? == 1,
            "下一轮应清掉剩余会话"
        );

        // 模拟候选读取后 refresh 更新 last_seen_at；删除必须复核谓词。
        insert_stale_batch(database.pool(), now, 1).await?;
        ensure!(
            delete_stale_candidates(database.pool(), vec!["stale-0".into()]).await? == 0,
            "已刷新候选不得删除"
        );
        sqlx::query("DELETE FROM user_session WHERE session_id = 'stale-0'")
            .execute(database.pool())
            .await?;

        insert_stale_batch(database.pool(), stale, 1001).await?;
        let (shutdown, receiver) = watch::channel(false);
        let mut worker = OwnedTask::new(tokio::spawn(run_loop(database.pool().clone(), receiver)));
        let drained = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_session")
                    .fetch_one(database.pool())
                    .await?;
                if remaining == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;
        let _ = shutdown.send(true);
        worker.join().await?;
        drained.context("积压清理不能每批等待六小时")??;
        sqlx::query("DROP TABLE user_session")
            .execute(database.pool())
            .await?;
        database.close().await;
        Ok(())
    }

    async fn insert_stale_batch(pool: &MySqlPool, stale: i64, count: usize) -> anyhow::Result<()> {
        let mut query = sqlx::QueryBuilder::<sqlx::MySql>::new(
            "INSERT INTO user_session (session_id,user_id,current_jti,created_at,last_seen_at,ip,user_agent) "
        );
        query.push_values(0..count, |mut row, id| {
            row.push_bind(format!("stale-{id}"))
                .push_bind(1)
                .push_bind(format!("jti-{id}"))
                .push_bind(stale)
                .push_bind(stale)
                .push_bind("127.0.0.1")
                .push_bind("cleanup-test");
        });
        query.build().execute(pool).await?;
        Ok(())
    }
}
