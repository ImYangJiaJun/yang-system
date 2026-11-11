//! 账号删除的跨 Addon 清理扩展点。
//!
//! account 域不能依赖飞书域，但 `user_identity` 有外键指向 `users`，绑定方
//! 又各自持有子表（例如飞书凭证与批量 job）。账号匿名化事务里必须先把这些
//! 子表清干净，否则要么撞外键、要么留下仍可用的旧令牌。这里只定义钩子，
//! 具体清理实现由持有子表的 Addon 在组合根注册。

use crate::db::Transaction;
use yang_base::action::ActionContext;
use yang_base::BaseError;

/// 账号删除事务内的 Addon 清理钩子。
#[async_trait::async_trait]
pub(crate) trait AccountDeletionHook: Send + Sync {
    async fn delete_user_data(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError>;
}
