//! 过期直授的后台收敛 Worker。
//!
//! 每 60 秒调用一次 [`GrantExpiryHandle::converge_once`]：把已过期但尚未收敛的
//! 直授行对应的用户授权版本递增并写 Outbox，使已签发的 Access Token 在一个
//! 收敛周期内失效，而不是等到 access TTL 自然过期。无积压时上界是一个周期；
//! 单轮最多 `GRANT_EXPIRY_BATCH` 行，积压时按批排空。
//!
//! 多实例通过用户授权行锁与 `expires_at` / `converged_at IS NULL` CAS 协作；
//! 仅成功消费该过期事实的实例递增版本并写 Outbox，不依赖蓝绿部署互斥。
//!
//! 与其它后台 Worker 同构：watch 通道触发退出；必须在 `Tools` 关闭前 shutdown，
//! 否则每轮只能拿到「Tools 已关闭」并空转刷错误日志。

use crate::addon::access::GrantExpiryHandle;
use anyhow::Context;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use yang_base::action::{ActionContext, Request};
use yang_base::tools::Tools;
use yang_runtime::shutdown::OwnedTask;

/// 收敛周期：无积压时过期行的存量 Token 最多再存活一个周期（加 Outbox 传播延迟）；
/// 有积压时按 `GRANT_EXPIRY_BATCH` 逐轮排空。
const GRANT_EXPIRY_INTERVAL: Duration = Duration::from_secs(60);

/// 单轮扫描上限；与 `converge_once` 的默认批量一致。
const GRANT_EXPIRY_BATCH: usize =
    crate::addon::access::domain::grant_expiry::DEFAULT_CONVERGENCE_LIMIT;

/// 与 HTTP 服务同生共死的过期直授收敛 Worker 句柄。
pub struct GrantExpiryWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
}

impl GrantExpiryWorker {
    pub fn start(tools: Arc<Tools>, handle: GrantExpiryHandle) -> anyhow::Result<Self> {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_loop(tools, handle, receiver));
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
            .context("等待过期直授收敛 Worker 退出失败")?;
        Ok(())
    }
}

impl Drop for GrantExpiryWorker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn run_loop(
    tools: Arc<Tools>,
    handle: GrantExpiryHandle,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), Arc::clone(&tools));
        match handle.converge_once(&ctx, GRANT_EXPIRY_BATCH).await {
            Ok(report) => {
                metrics::counter!("authz_grant_expiry_runs_total", "result" => "success")
                    .increment(1);
                metrics::counter!("authz_grant_expiry_converged_total")
                    .increment(report.converged as u64);
                metrics::counter!("authz_grant_expiry_users_total").increment(report.users as u64);
                metrics::gauge!("authz_grant_expiry_pending")
                    .set(report.scanned.saturating_sub(report.converged) as f64);
                if report.converged > 0 {
                    tracing::info!(
                        scanned = report.scanned,
                        users = report.users,
                        converged = report.converged,
                        "过期直授收敛完成"
                    );
                }
            }
            Err(error) => {
                metrics::counter!("authz_grant_expiry_runs_total", "result" => "error")
                    .increment(1);
                tracing::error!(error = %error, "过期直授收敛失败");
            }
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            () = tokio::time::sleep(GRANT_EXPIRY_INTERVAL) => {}
        }
    }
}
