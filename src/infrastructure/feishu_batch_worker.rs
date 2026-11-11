//! 飞书批量审批的后台执行 Worker。
//!
//! 与授权收敛 Worker 同构：`watch` 通道触发退出，必须在 `Tools` 关闭前 shutdown。
//! 每轮先把进程中断遗留的 running item 标成 unknown 并核对详情，再为 pending /
//! running job 拉起执行器；执行器内部按 item 串行，写请求不自动重放。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
use yang_base::action::{ActionContext, Request};
use yang_base::tools::Tools;
use yang_runtime::shutdown::OwnedTask;

use crate::addon::feishu::batch_approval::worker::run_due_jobs;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::config::FeishuSettings;

pub struct FeishuBatchWorker {
    shutdown: watch::Sender<bool>,
    task: OwnedTask<()>,
}

impl FeishuBatchWorker {
    pub fn start(
        tools: Arc<Tools>,
        context: Arc<FeishuContext>,
        settings: Arc<FeishuSettings>,
    ) -> anyhow::Result<Self> {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_loop(tools, context, settings, receiver));
        Ok(Self {
            shutdown,
            task: OwnedTask::new(task),
        })
    }

    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.task.is_finished()
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        self.task
            .join()
            .await
            .context("等待飞书批量审批 Worker 退出失败")?;
        Ok(())
    }
}

impl Drop for FeishuBatchWorker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn run_loop(
    tools: Arc<Tools>,
    context: Arc<FeishuContext>,
    settings: Arc<FeishuSettings>,
    mut shutdown: watch::Receiver<bool>,
) {
    let interval = Duration::from_secs(settings.batch_worker_interval_seconds.max(1));
    loop {
        if *shutdown.borrow() {
            break;
        }
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), Arc::clone(&tools));
        match run_due_jobs(
            &ctx,
            Arc::clone(&context),
            Arc::clone(&settings),
            settings.batch_max_tasks as usize,
        )
        .await
        {
            Ok(0) => {}
            Ok(started) => {
                metrics::counter!("feishu_batch_worker_jobs_total").increment(started as u64);
                tracing::info!(started, "批量审批 job 已启动");
            }
            Err(error) => {
                metrics::counter!("feishu_batch_worker_errors_total").increment(1);
                tracing::error!(error = %error, "批量审批 Worker 轮询失败");
            }
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}
