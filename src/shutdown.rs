//! 进程级关闭总预算。
//!
//! 所有关闭阶段共享同一个绝对截止时间，避免多个独立 timeout 串联后突破
//! 运维给进程预留的终止窗口。

use anyhow::bail;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::{timeout_at, Instant};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ShutdownTrigger {
    Signal,
    OperationExit,
}

impl ShutdownTrigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Signal => "signal",
            Self::OperationExit => "operation_exit",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ShutdownPhase {
    HttpDrain,
    AuthorizationOutboxWorker,
    ToolsClose,
}

impl ShutdownPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::HttpDrain => "http_drain",
            Self::AuthorizationOutboxWorker => "authorization_outbox_worker",
            Self::ToolsClose => "tools_close",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ShutdownBudget {
    total: Duration,
    shared: Arc<SharedState>,
}

struct SharedState {
    window: Mutex<Option<ShutdownWindow>>,
    started: Notify,
}

#[derive(Debug, Clone, Copy)]
struct ShutdownWindow {
    started_at: Instant,
    deadline: Instant,
}

impl ShutdownBudget {
    pub(crate) fn new(total: Duration) -> Self {
        Self {
            total,
            shared: Arc::new(SharedState {
                window: Mutex::new(None),
                started: Notify::new(),
            }),
        }
    }

    /// 幂等地启动关闭窗口；先到达的触发源决定唯一截止时间。
    pub(crate) async fn begin(&self, trigger: ShutdownTrigger) -> Instant {
        let mut guard = self.shared.window.lock().await;
        if let Some(window) = *guard {
            return window.deadline;
        }

        let started_at = Instant::now();
        let deadline = started_at + self.total;
        *guard = Some(ShutdownWindow {
            started_at,
            deadline,
        });
        drop(guard);

        self.shared.started.notify_waiters();
        metrics::counter!(
            "shutdown_started_total",
            "trigger" => trigger.as_str()
        )
        .increment(1);
        tracing::info!(
            trigger = trigger.as_str(),
            total_budget_ms = self.total.as_millis() as u64,
            "进程关闭总预算已启动"
        );
        deadline
    }

    /// 等待关闭窗口启动。先注册通知再检查状态，避免丢失并发通知。
    pub(crate) async fn wait_started(&self) -> Instant {
        loop {
            let notified = self.shared.started.notified();
            if let Some(window) = *self.shared.window.lock().await {
                return window.deadline;
            }
            notified.await;
        }
    }

    pub(crate) async fn run_phase<T, F>(&self, phase: ShutdownPhase, future: F) -> anyhow::Result<T>
    where
        F: Future<Output = anyhow::Result<T>>,
    {
        let window = loop {
            if let Some(window) = *self.shared.window.lock().await {
                break window;
            }
            self.wait_started().await;
        };
        let phase_name = phase.as_str();
        let phase_started = Instant::now();
        let remaining_at_start = window.deadline.saturating_duration_since(phase_started);
        tracing::info!(
            phase = phase_name,
            elapsed_total_ms = phase_started
                .saturating_duration_since(window.started_at)
                .as_millis() as u64,
            remaining_budget_ms = remaining_at_start.as_millis() as u64,
            "开始执行关闭阶段"
        );

        match timeout_at(window.deadline, future).await {
            Ok(Ok(value)) => {
                self.record_phase(phase, "success", phase_started, window.deadline);
                Ok(value)
            }
            Ok(Err(error)) => {
                self.record_phase(phase, "error", phase_started, window.deadline);
                tracing::error!(
                    phase = phase_name,
                    error = %error,
                    "关闭阶段执行失败"
                );
                Err(error)
            }
            Err(_) => {
                self.record_phase(phase, "timeout", phase_started, window.deadline);
                tracing::error!(
                    phase = phase_name,
                    total_budget_ms = self.total.as_millis() as u64,
                    "关闭阶段耗尽进程总预算"
                );
                bail!(
                    "关闭阶段 {phase_name} 耗尽进程总预算（{} ms）",
                    self.total.as_millis()
                )
            }
        }
    }

    fn record_phase(
        &self,
        phase: ShutdownPhase,
        result: &'static str,
        phase_started: Instant,
        deadline: Instant,
    ) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(phase_started);
        metrics::counter!(
            "shutdown_phase_total",
            "phase" => phase.as_str(),
            "result" => result
        )
        .increment(1);
        metrics::histogram!(
            "shutdown_phase_duration_seconds",
            "phase" => phase.as_str()
        )
        .record(elapsed.as_secs_f64());
        tracing::info!(
            phase = phase.as_str(),
            result,
            elapsed_phase_ms = elapsed.as_millis() as u64,
            remaining_budget_ms = deadline.saturating_duration_since(now).as_millis() as u64,
            "关闭阶段结束"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn phases_share_one_deadline_instead_of_accumulating_timeouts() {
        let budget = ShutdownBudget::new(Duration::from_millis(120));
        budget.begin(ShutdownTrigger::Signal).await;

        budget
            .run_phase(ShutdownPhase::HttpDrain, async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(())
            })
            .await
            .unwrap_or_else(|error| panic!("第一阶段应在总预算内完成: {error:#}"));

        let error = budget
            .run_phase(ShutdownPhase::AuthorizationOutboxWorker, async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(())
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("第二阶段必须只获得剩余预算"));
        assert!(error.to_string().contains("authorization_outbox_worker"));
    }

    #[tokio::test]
    async fn first_trigger_owns_the_single_deadline() {
        let budget = ShutdownBudget::new(Duration::from_secs(1));
        let signal_deadline = budget.begin(ShutdownTrigger::Signal).await;
        tokio::task::yield_now().await;
        let operation_deadline = budget.begin(ShutdownTrigger::OperationExit).await;

        assert_eq!(signal_deadline, operation_deadline);
        assert_eq!(budget.wait_started().await, signal_deadline);
    }
}
