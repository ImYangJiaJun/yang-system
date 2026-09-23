//! 账号域两个跨域端口的 access 实现：引导首个注册账号为系统管理员，以及
//! 账号生命周期所需的授权事实（最后管理员判定与授权事实清理）。
//!
//! 并发仲裁完全交给 `system_owner` 表的唯一约束——**不做任何「判空」**。
//! 「用户表为空则本次注册者晋升」是典型 TOCTOU，在并发下会产出多个管理员。

use super::admin::{count_active_system_admins_in_tx, invalidate_users_in_tx};
use super::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::addon::access::domain::context::Access;
use crate::addon::account::{OwnerClaimOutcome, SystemAuthorizationPort, SystemOwnerClaimer};
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

/// 账号生命周期守卫的 access 侧实现：只有本域持有组事实的受信 writer。
///
/// 与 `SystemOwnerClaimer` 复用同一个类型，是因为两者都只需要 `Arc<Access>`：
/// 再拆一个结构体会重复同一份装配代码，却不增加任何隔离。
#[async_trait]
impl SystemAuthorizationPort for AccessSystemOwnerClaimer {
    async fn remains_an_admin_after(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        target_user_id: i64,
    ) -> Result<bool, BaseError> {
        let admins = count_active_system_admins_in_tx(&self.access, ctx, transaction).await?;
        let Some(group) = self
            .access
            .groups()
            .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
            .await?
        else {
            // 内置全权组不存在时系统本就没有管理员，任何账号操作都不会让这个
            // 不变量变得更糟；保守起见仍按「不能再少」处理。
            return Ok(false);
        };
        let members = self
            .access
            .groups()
            .list_members_in_tx(ctx, transaction, group.id)
            .await?;
        if !members.contains(&target_user_id) {
            // 目标本就不是管理员，任何操作都不影响该不变量。
            return Ok(true);
        }
        Ok(admins > 1)
    }

    async fn purge_user_facts_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError> {
        self.access
            .grants()
            .delete_all_of_user_in_tx(ctx, transaction, user_id)
            .await?;
        self.access
            .groups()
            .delete_member_rows_of_user_in_tx(ctx, transaction, user_id)
            .await?;
        Ok(())
    }
}
