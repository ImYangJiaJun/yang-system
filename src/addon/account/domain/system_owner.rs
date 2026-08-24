//! 首个注册账号声明系统最终管理员的持久化端口。

use async_trait::async_trait;
use yang_base::BaseError;
use yang_db::Transaction;

/// 系统最终管理员声明结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerClaimOutcome {
    /// 当前注册事务成功声明了唯一最终管理员。
    #[cfg_attr(not(feature = "admin"), allow(dead_code))]
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

/// 不声明最终管理员的默认声明器。
///
/// `admin` Addon 未随 feature 启用时由组合根注入：注册流程照常完成，
/// 但任何账号都不会成为系统最终管理员。
#[cfg(not(feature = "admin"))]
pub(crate) struct NoSystemOwnerClaimer;

#[cfg(not(feature = "admin"))]
#[async_trait]
impl SystemOwnerClaimer for NoSystemOwnerClaimer {
    async fn claim(
        &self,
        _transaction: &mut Transaction,
        _user_id: i64,
        _username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        Ok(OwnerClaimOutcome::AlreadyClaimed)
    }
}
