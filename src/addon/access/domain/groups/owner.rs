//! `SystemOwnerClaimer` 的 access 实现：引导首个注册账号为系统管理员。
//!
//! 并发仲裁完全交给 `system_owner` 表的唯一约束——**不做任何「判空」**。
//! 「用户表为空则本次注册者晋升」是典型 TOCTOU，在并发下会产出多个管理员。

use super::admin::invalidate_users_in_tx;
use crate::addon::access::domain::context::Access;
use crate::addon::account::{OwnerClaimOutcome, SystemOwnerClaimer};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 把首个成功竞争到引导哨兵的注册账号提升为系统管理员。
pub(crate) struct AccessSystemOwnerClaimer {
    access: Arc<Access>,
}

impl AccessSystemOwnerClaimer {
    pub(crate) fn new(access: Arc<Access>) -> Self {
        Self { access }
    }
}

#[async_trait]
impl SystemOwnerClaimer for AccessSystemOwnerClaimer {
    async fn claim(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        username: &str,
    ) -> Result<OwnerClaimOutcome, BaseError> {
        // 1. 竞争哨兵：唯一约束 + CHECK 让第二个插入者必然失败。
        match self
            .access
            .groups()
            .insert_owner_sentinel_in_tx(ctx, transaction, user_id)
            .await
        {
            Ok(()) => {}
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Ok(OwnerClaimOutcome::AlreadyClaimed);
            }
            Err(error) => return Err(error),
        }

        // 2. 夺到哨兵：加入内置全权组并使其 Token 生效。
        let group_id = self
            .access
            .groups()
            .ensure_system_admin_group_in_tx(ctx, transaction)
            .await?;
        self.access
            .groups()
            .insert_member_in_tx(ctx, transaction, user_id, group_id, user_id)
            .await?;
        let affected = BTreeSet::from([user_id]);
        invalidate_users_in_tx(&self.access, ctx, transaction, &affected).await?;

        tracing::info!(user_id, username, "首个注册账号已引导为系统管理员");
        Ok(OwnerClaimOutcome::Claimed { admin_id: user_id })
    }
}
