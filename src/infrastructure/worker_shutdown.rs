//! worker 轮次与关闭信号的竞争。
//!
//! 关闭预算（`ShutdownBudget`）是所有 worker 共享的固定时长；如果一轮业务处理
//! 在预算内结束不了，进程会以失败退出码收尾（部署时的真实形态：ExitCode=1）。
//! 这里让「进行中的轮次」与关闭信号赛跑：关闭先到就放弃本轮，让 worker 尽快退出。
//!
//! 取消安全性：审批派发的轮次以租约（`lease_until`）与 `effect_phase` 状态机
//! 设计为可重启（见 `approval_execution`），事件轮次逐条标记处理结果；放弃
//! 进行中的轮次最多留下「本轮未处理完」的行，启动后会重新认领，不会丢数据。

use std::future::Future;

use tokio::sync::watch;

/// 等待一轮结束；关闭信号先到则放弃本轮并返回 `false`。
///
/// `shutdown` 只会在 `request_shutdown` / `Drop` 里 `send(true)`，所以
/// `changed()` 的任何结果（Ok 或发送端已丢弃的 Err）都按「需要关闭」处理。
/// `biased` 让关闭分支优先求值：已经收到信号时不再先轮询业务 future。
pub(crate) async fn run_round_or_shutdown<F>(mut shutdown: watch::Receiver<bool>, round: F) -> bool
where
    F: Future<Output = ()>,
{
    tokio::select! {
        biased;
        _ = shutdown.changed() => false,
        () = round => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn shutdown_cancels_an_inflight_round() {
        let (sender, shutdown) = watch::channel(false);
        let round = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        };
        let trigger = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = sender.send(true);
        };
        // 变异守护：若 select 去掉关闭分支，这里会等满 30 秒并被 2 秒超时打断。
        let (completed, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(run_round_or_shutdown(shutdown, round), trigger)
        })
        .await
        .unwrap_or_else(|error| panic!("关闭信号必须能取消进行中的轮次: {error:?}"));
        assert!(!completed, "被取消的轮次应返回 false");
    }

    #[tokio::test]
    async fn completed_round_is_not_marked_cancelled() {
        let (_sender, shutdown) = watch::channel(false);
        assert!(
            run_round_or_shutdown(shutdown, async {}).await,
            "正常结束的轮次返回 true"
        );
    }

    #[tokio::test]
    async fn already_requested_shutdown_returns_immediately() {
        let (sender, shutdown) = watch::channel(false);
        sender
            .send(true)
            .unwrap_or_else(|error| panic!("receiver 仍在，发送应成功: {error:?}"));
        let completed = tokio::time::timeout(
            Duration::from_secs(1),
            run_round_or_shutdown(shutdown, async {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("已请求关闭时应立即返回: {error:?}"));
        assert!(!completed);
    }
}
