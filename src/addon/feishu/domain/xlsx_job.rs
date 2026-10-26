//! XLSX 阻塞工作的所有权：请求取消只发信号，实际任务结束才释放资源。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::OwnedSemaphorePermit;
use yang_base::BaseError;

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// 成功后将资源交回调用方，导入写库阶段继续持有同一份 permit/ImportGuard。
/// JoinHandle 被请求取消而丢弃时，闭包仍持有资源，直到协作退出或正常结束。
pub(crate) async fn run<T, G>(
    permit: OwnedSemaphorePermit,
    guard: G,
    work: impl FnOnce(&AtomicBool) -> Result<T, BaseError> + Send + 'static,
) -> Result<(T, G, OwnedSemaphorePermit), BaseError>
where
    T: Send + 'static,
    G: Send + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(Arc::clone(&cancelled));
    tokio::task::spawn_blocking(move || {
        let value = work(&cancelled)?;
        Ok((value, guard, permit))
    })
    .await
    .map_err(|error| BaseError::ConfigError(format!("xlsx 解析任务失败：{error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::feishu::domain::import_progress::{self, ImportGuard};
    use tokio::sync::Semaphore;

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_blocking_work_keeps_the_slot_and_import_guard_until_it_exits() {
        let slots = Arc::new(Semaphore::new(1));
        for datasource_id in [-6101, -6103] {
            let permit = Arc::clone(&slots)
                .try_acquire_owned()
                .unwrap_or_else(|_| panic!("空闲"));
            let guard =
                ImportGuard::acquire_for_test(datasource_id).unwrap_or_else(|_| panic!("空闲"));
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (cancel_seen_tx, cancel_seen_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let request = tokio::spawn(run(permit, guard, move |cancelled| {
                let _ = entered_tx.send(());
                let started = std::time::Instant::now();
                while !cancelled.load(Ordering::Acquire) {
                    if started.elapsed() > std::time::Duration::from_secs(3) {
                        return Err(BaseError::ConfigError("未收到取消".to_string()));
                    }
                    std::thread::yield_now();
                }
                let _ = cancel_seen_tx.send(());
                // 可控的外部阻塞点：信号已收到，但实际闭包尚未结束。
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|error| BaseError::ConfigError(error.to_string()))?;
                Ok(())
            }));
            tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
                .await
                .unwrap_or_else(|_| panic!("blocking 任务没有启动"))
                .unwrap_or_else(|_| panic!("任务提前退出"));
            // current_thread 上仍能驱动异步心跳，证明 CPU/阻塞工作没有占着运行时线程。
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            request.abort();
            let _ = request.await;
            tokio::time::timeout(std::time::Duration::from_secs(5), cancel_seen_rx)
                .await
                .unwrap_or_else(|_| panic!("取消没有传给 blocking 任务"))
                .unwrap_or_else(|_| panic!("任务提前退出"));
            assert!(
                Arc::clone(&slots).try_acquire_owned().is_err(),
                "取消不能提前释放全局容量"
            );
            assert!(
                ImportGuard::acquire_for_test(datasource_id).is_err(),
                "取消不能提前放行同源导入"
            );
            assert!(
                import_progress::snapshot(datasource_id).is_some(),
                "仍在工作的任务不能显示 idle"
            );
            release_tx
                .send(())
                .unwrap_or_else(|_| panic!("任务应仍在等待"));
            let _next = tokio::time::timeout(std::time::Duration::from_secs(5), slots.acquire())
                .await
                .unwrap_or_else(|_| panic!("结束后应释放容量"))
                .unwrap_or_else(|_| panic!("容量不应关闭"));
            assert!(
                ImportGuard::acquire_for_test(datasource_id).is_ok(),
                "结束后应释放同源占用"
            );
        }
    }

    #[tokio::test]
    async fn successful_parse_keeps_resources_owned_through_the_async_write_phase() {
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&slots)
            .try_acquire_owned()
            .unwrap_or_else(|_| panic!("空闲"));
        let guard = ImportGuard::acquire_for_test(-6102).unwrap_or_else(|_| panic!("空闲"));
        let completed = run(permit, guard, |_| Ok(7))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(completed.0, 7);
        tokio::task::yield_now().await;
        assert!(Arc::clone(&slots).try_acquire_owned().is_err());
        assert!(ImportGuard::acquire_for_test(-6102).is_err());
        drop(completed);
        assert!(Arc::clone(&slots).try_acquire_owned().is_ok());
        assert!(ImportGuard::acquire_for_test(-6102).is_ok());
    }
}
