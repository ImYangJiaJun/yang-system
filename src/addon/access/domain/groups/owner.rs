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
        // 计数与「目标是否计入」出自同一次加锁读取：调用方据此判断去掉目标之后
        // 是否还剩人（spec §8.2 的守卫语义是「操作之后仍有至少一名启用管理员」，
        // 不是「当前至少有两名」）。
        let admins =
            count_active_system_admins_in_tx(&self.access, ctx, transaction, target_user_id)
                .await?;
        if !admins.group_exists() {
            // 内置全权组不存在时系统本就没有管理员，任何账号操作都不会让这个
            // 不变量变得更糟；保守起见仍按「不能再少」处理。
            return Ok(false);
        }
        Ok(admins.keeps_at_least_one_admin())
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

    async fn ensure_operator_may_modify_system_admin_member(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        operator_id: i64,
        target_user_id: i64,
    ) -> Result<(), BaseError> {
        // 与组成员 Action 的守卫同源：按 `group_key` 找到内置全权组，读它的成员名单，
        // 再判「目标在名单内、而操作者不在」。这里刻意复用
        // `count_active_system_admins_in_tx` 所用的同一对仓库读（`find_by_key_in_tx` +
        // `list_members_in_tx`），不另造一套成员查询；也刻意**不**取组行锁——账号生命
        // 周期路径与成员变更同向（都先锁目标 users 行），设计 §6.3 为本类路径记的例外
        // 同样适用于本守卫（完整论证见 `admin.rs` 模块注释与
        // `count_active_system_admins_in_tx`）。
        let Some(group) = self
            .access
            .groups()
            .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
            .await?
        else {
            // 内置全权组不存在 ⇒ 系统里没有「全权组成员」需要保护，放行。
            return Ok(());
        };
        let members = self
            .access
            .groups()
            .list_members_in_tx(ctx, transaction, group.id)
            .await?;
        if members.contains(&target_user_id) && !members.contains(&operator_id) {
            return Err(system_admin_member_guard());
        }
        Ok(())
    }
}

/// 「只有全权组成员能修改全权组成员」的统一拒绝（spec §8.1 附加规则）。
///
/// 与 `add_group_member` / `remove_group_member` 里那句逐字相同：三条路径必须给出同一
/// 句文案，客户端才能用同一个判据认出撞上的是哪条守卫。消息同样只说明规则，不泄漏
/// 「目标是不是管理员」这类授权事实。
fn system_admin_member_guard() -> BaseError {
    BaseError::PermissionDenied("只有系统管理员可以修改系统管理员组的成员".to_string())
}
