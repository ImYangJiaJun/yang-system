//! 从权限组移除一条权限（幂等）。
//!
//! # 锁序
//!
//! 全仓统一的锁序纪律是 `users 行升序 → 组行`（见 `domain::groups::admin` 模块注释）。
//! `add_group_item` 与 `remove_group_member` 都遵守这个顺序。本 Action 与其同向，避免在
//! 组行与 users 行上构成 ABBA 死锁。
//!
//! 具体步骤：
//! 1. 事务外：探查组成员列表（只读 SELECT，不锁定），确定下个事务的 users 行锁集合。
//! 2. 开事务。
//! 3. 锁 users 行升序（`lock_users_ascending_in_tx`）。
//! 4. 锁 group 行（`lock_group_in_tx`）。
//! 5. 重读成员列表（此时快照必然包含先于本事务取到这批锁的全部提交）。
//! 6. 删除条目（`delete_item_in_tx`）。
//! 7. 扇出 invalidate（`invalidate_users_in_tx`，只对已持锁的成员）。
//! 8. 审计事件。
//! 9. 提交事务。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{
    ensure_member_limit, ensure_operator_may_manage_group, invalidate_users_in_tx,
    lock_users_ascending_in_tx, GROUP_WRITE_PERMISSION,
};
use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::addon::access::domain::permission_catalog::{PERMISSION_MAX_LENGTH, PERMISSION_PATTERN};
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RemoveGroupItemInput {
        group_id: Key::new()
            .title("权限组")
            .require(true),
        permission: Str::new()
            .title("权限")
            .require(true)
            .min_length(3)
            .max_length(PERMISSION_MAX_LENGTH)
            .pattern(PERMISSION_PATTERN),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct RemoveGroupItemResult {
    group_id: i64,
    permission: String,
    /// 本次是否真的删除了条目：组内本就没有该权限时为 `false`，既不递增任何人的
    /// 授权版本，也不写授权 Outbox（spec §9.2 的幂等语义）。
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RemoveGroupItemInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    // 移除**不做**目录成员校验：已从 Catalog 移除的权限也必须能清理，否则孤儿条目
    // 永远清不掉（对齐 revoke_permission 的反向宽容语义）。

    // 第一步（在事务之外）：只读探查一次组成员，**只**用来确定下一个事务的行锁集合。
    //
    // 与 `add_group_item` 同一手法、同一理由：本事务的第一条语句必须是 users 行锁，
    // 但锁集又必须先知道成员集合才知道锁谁。于是把这次「只为定锁集」的探查放到事务之外。
    // 探查可以陈旧：少算一行只是少锁一行（扇出交集把它折算成欠授权，不影响正确性），
    // 多算一行只是多重锁一行。
    let probed_members = {
        let mut probe = ctx.tools().mysql()?.transaction().await?;
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut probe, input.group_id)
            .await?;
        Access::finish_transaction(probe, Ok(())).await?;
        members
    };
    // 上限先于行锁：超限时不得进入 O(N) 行锁事务（spec §6.3）。
    ensure_member_limit(probed_members.len() as u64)?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 第二步：本事务的**第一条语句**就是这批行锁——按 `user_id` 升序一次性锁住
        // {操作者} ∪ {探查到的组成员}。
        let mut lock_set: BTreeSet<i64> = probed_members.into_iter().collect();
        lock_set.insert(operator_id);
        lock_users_ascending_in_tx(&access, &ctx, &mut transaction, &lock_set).await?;

        // 第三步：锁 group 行（`lock_group_in_tx`，`permission_group` 主键上的 `FOR UPDATE`
        // 记录锁）。此时不会死锁——因为 users 行已先锁，与 `add_group_item` 同向。
        let group = access
            .groups()
            .lock_group_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
        // 内置全权组的权限由权限目录计算，条目表里根本没有可删的事实。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            return Err(BaseError::ParamInvalid(
                "group_id".to_string(),
                "内置系统管理员组的权限由权限目录计算，不能增删条目".to_string(),
            ));
        }
        // 组所有者语义：操作者须是组所有者、或持有全局写权限（claims）。
        // 判据只读 `created_by` 与 claims，不新增库读；判定建立在锁后组事实之上。
        ensure_operator_may_manage_group(
            operator_id,
            &group,
            ctx.authenticated_user()
                .is_some_and(|user| user.has_permission(GROUP_WRITE_PERMISSION)),
        )?;

        // 锁后读成员名单（本事务的第一条普通 SELECT，快照在此建立——必然包含先于本事务
        // 取到这批锁的全部提交）。
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        // 上限在加锁前已判定过一次；这里再按锁后快照复核一次。
        ensure_member_limit(members.len() as u64)?;

        let changed = access
            .groups()
            .delete_item_in_tx(&ctx, &mut transaction, group.id, &input.permission)
            .await?;
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写 Outbox。
        }

        // 扇出失效：只对本次已按升序持锁的成员行递增版本。
        // 探查之后才被加入本组的成员不在此集合里，这是安全的：加入它的
        // `add_group_member` 自身已经处理过它的授权版本。
        let affected: BTreeSet<i64> = members
            .iter()
            .copied()
            .filter(|user_id| lock_set.contains(user_id))
            .collect();
        invalidate_users_in_tx(&access, &ctx, &mut transaction, &affected).await?;

        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group.id)?,
            Some(audit::summary([("permission", json!(input.permission))])?),
            None,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(true)
    }
    .await;
    let changed = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        RemoveGroupItemResult {
            group_id: input.group_id,
            permission: input.permission,
            changed,
        },
        if changed {
            "权限已从组中移除，组成员刷新会话后失去该权限"
        } else {
            "该组本就没有该权限"
        },
    )
}

/// 自包含注册：路由/认证声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——组所有者或全局写权限持有者可管理
    // （handler 内判定，非所有者一律 403）
    module
        .action_fn(
            yang_base::action_name!("remove_group_item"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/items/remove")
        .display_name("移除组权限")
        .description("从权限组移除一条权限（幂等；已不在目录中的孤儿条目同样可清理）")
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_id_and_permission_format_contract() {
        let injected = serde_json::from_value::<RemoveGroupItemInput>(serde_json::json!({
            "group_id": 3,
            "permission": "access.grants.read",
            "reason": "cleanup"
        }));
        assert!(injected.is_err(), "客户端不能注入额外字段");

        let without_group = serde_json::from_value::<RemoveGroupItemInput>(
            serde_json::json!({ "permission": "access.grants.read" }),
        );
        assert!(without_group.is_err(), "缺少 group_id 必须被拒绝");

        let params = <RemoveGroupItemInput as ParamInput>::params();
        let param = |name: &str| {
            params
                .as_slice()
                .iter()
                .find(|param| param.name.as_str() == name)
                .unwrap_or_else(|| panic!("应声明 {name} 参数"))
        };
        assert!(param("group_id").required);
        assert_eq!(
            param("permission").validation.pattern.as_deref(),
            Some(PERMISSION_PATTERN)
        );
    }
}
