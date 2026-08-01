//! 首个注册账号声明系统最终管理员的持久化端口。

use async_trait::async_trait;
use yang_base::BaseError;
use yang_db::Transaction;

/// 系统最终管理员声明结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerClaimOutcome {
    /// 当前注册事务成功声明了唯一最终管理员。
    Claimed { admin_id: i64 },
    /// 最终管理员已经由另一个已提交或正在提交的事务声明。
    AlreadyClaimed,
}

/// 由平台账号域实现的最终管理员声明端口。
#[async_trait]
pub(crate) trait SystemOwnerClaimer: Send + Sync {
    /// 在创建用户的同一事务中竞争唯一最终管理员哨兵。
    async fn claim(
        &self,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError>;
}
