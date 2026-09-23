//! 账号域对外暴露的两个跨域端口：首个注册账号的引导声明，以及账号生命周期
//! 所需的授权事实（最后管理员不变量与账号事实清理）。

use async_trait::async_trait;
use yang_base::action::ActionContext;
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
    ///
    /// `ctx` 是必需参数：实现方必须经受信 writer 写入授权事实，
    /// 而 writer 需要 `ctx` 取得连接池（`trusted_query`）。
    async fn claim(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError>;
}

/// account 生命周期所需的授权事实端口：由 access 域实现。
///
/// 存在的理由是依赖方向：account 不能依赖 access（会成环），因此把
/// 「系统管理员不变量」与「账号事实清理」这两个跨域操作抽成端口，
/// 经组合根注入——与 `SystemOwnerClaimer`、`GrantResolver` 同一模式。
#[async_trait]
pub(crate) trait SystemAuthorizationPort: Send + Sync {
    /// 目标用户在「被停用/被删除/被移出全权组」之后，系统是否仍有
    /// 至少一名启用的系统管理员。返回 `false` 表示该操作必须被拒绝。
    async fn remains_an_admin_after(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        target_user_id: i64,
    ) -> Result<bool, BaseError>;

    /// 账号删除时清理其授权事实（`authz_grant` 直授与 `user_group` 成员行）。
    ///
    /// 必须经 access 的 writer 方法完成，**不得**在 account 侧直写这两张表，
    /// 否则绕过 `docs/architecture/authorization-writers.md` 的 writer 边界。
    async fn purge_user_facts_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError>;
}
