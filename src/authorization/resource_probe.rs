//! 资源授权线性化并发测试探针。

use std::sync::Arc;
use tokio::sync::Barrier;
use yang_base::action::ActionContext;

/// 资源授权竞争测试可暂停的确定边界。
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceAuthorizationCheckpoint {
    /// middleware 已完成事务外快速预检，但业务事务尚未开始。
    AfterPrecheck,
    /// 业务事务已锁定并复核当前管理员事实。
    AfterLinearization,
}

/// 仅由集成测试注入 `Tools` 的双屏障探针。
#[doc(hidden)]
#[derive(Clone)]
pub struct ResourceAuthorizationProbe {
    checkpoint: ResourceAuthorizationCheckpoint,
    reached: Arc<Barrier>,
    resume: Arc<Barrier>,
}

impl ResourceAuthorizationProbe {
    /// 创建只在指定边界暂停一次的探针。
    pub fn new(checkpoint: ResourceAuthorizationCheckpoint) -> Self {
        Self {
            checkpoint,
            reached: Arc::new(Barrier::new(2)),
            resume: Arc::new(Barrier::new(2)),
        }
    }

    /// 等待被测请求到达指定边界。
    pub async fn wait_until_reached(&self) {
        self.reached.wait().await;
    }

    /// 允许被测请求越过暂停边界。
    pub async fn resume(&self) {
        self.resume.wait().await;
    }

    async fn pause_if_selected(&self, checkpoint: ResourceAuthorizationCheckpoint) {
        if self.checkpoint == checkpoint {
            self.reached.wait().await;
            self.resume.wait().await;
        }
    }
}

pub(crate) async fn checkpoint(ctx: &ActionContext, checkpoint: ResourceAuthorizationCheckpoint) {
    if let Ok(probe) = ctx.tools().extension::<ResourceAuthorizationProbe>() {
        probe.pause_if_selected(checkpoint).await;
    }
}
