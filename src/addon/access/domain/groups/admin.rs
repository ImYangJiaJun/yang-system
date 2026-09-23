//! 组管理的事务编排：扇出失效、提权校验、最后管理员判定与成员上限。
//!
//! 这些机制被多个 Action 共用，集中在此以保证语义单一——尤其
//! `effective_permissions_of_in_tx` 必须与 `GroupGrantResolver` 走同一
//! 解析函数（spec §8.1）。

// 消费者（组管理 Action 在 Task 10+、账号生命周期守卫在 Task 13）晚于本任务
// 接入，与 `repository.rs` / `resolution.rs` 同例显式豁免 dead-code 门禁。
#![allow(dead_code)]

use super::repository::{MAX_GROUP_MEMBERS, SYSTEM_ADMIN_GROUP_KEY};
use super::resolution::{catalog_permissions, resolve_group_permissions};
use crate::addon::access::domain::context::Access;
use std::collections::BTreeSet;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 使受影响用户的 Access Token 失效（spec §6.3）。
///
/// **锁序**：必须按 `user_id` 升序加锁，这是防死锁的唯一手段。
/// `BTreeSet` 的迭代顺序天然有序，不要改成 `HashSet`。
pub(crate) async fn invalidate_users_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    affected: &BTreeSet<i64>,
) -> Result<(), BaseError> {
    for user_id in affected {
        let locked = access
            .authorization()
            .lock_authorization_version(ctx.tools().mysql()?.pool(), transaction, *user_id)
            .await?;
        if !locked.is_active() {
            // 已停用用户的 Token 本就不可用，无需递增版本。
            continue;
        }
        access
            .authorization()
            .increment_locked_authorization_version(transaction, &locked)
            .await?;
    }
    Ok(())
}

/// 某用户的当前有效权限（供提权校验使用，与 resolvers 共用解析函数）。
pub(crate) async fn effective_permissions_of_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    user_id: i64,
) -> Result<BTreeSet<String>, BaseError> {
    let catalog = catalog_permissions(access.permission_catalog())?;
    let mut effective: BTreeSet<String> = BTreeSet::new();
    // 直授权限同样计入——提权判定看的是「有效权限」而非仅组权限。
    for record in access
        .grants()
        .list_by_user_in_tx(ctx, transaction, user_id)
        .await?
    {
        if catalog.contains(&record.permission) {
            effective.insert(record.permission);
        }
    }
    for group_id in access
        .groups()
        .list_group_ids_of_user_in_tx(ctx, transaction, user_id)
        .await?
    {
        let Some(group) = access
            .groups()
            .find_by_id_in_tx(ctx, transaction, group_id)
            .await?
        else {
            continue;
        };
        let items = access
            .groups()
            .list_items_in_tx(ctx, transaction, group_id)
            .await?;
        effective.extend(resolve_group_permissions(
            &group.group_key,
            &items,
            &catalog,
        ));
    }
    Ok(effective)
}

/// spec §8.1 的不变量：任何组管理操作都不得使调用者自身有效权限增大。
pub(crate) fn assert_no_self_escalation(
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
) -> Result<(), BaseError> {
    if after.is_subset(before) {
        return Ok(());
    }
    let added: Vec<&str> = after.difference(before).map(String::as_str).collect();
    Err(BaseError::PermissionDenied(format!(
        "该操作会使你自己的权限增加（{}），已拒绝",
        added.join(", ")
    )))
}

/// 组成员数上限检查；超过上限必须明确报错而不是静默做 O(N) 行锁事务。
///
/// 设计 §9.3 把这一场景列为 `Conflict`/409，但 `yang_base::BaseError` 并无
/// `Conflict` 变体，且同节明确要求「不为个别用例扩展框架错误类型」。因此复用
/// 既有的 `ParamInvalid`——与「用户名已存在」等业务前置拒绝同一条路径，
/// 消息里同时给出当前成员数、上限与处置办法。
pub(crate) fn ensure_member_limit(member_count: u64) -> Result<(), BaseError> {
    if member_count > MAX_GROUP_MEMBERS as u64 {
        return Err(BaseError::ParamInvalid(
            "member_count".to_string(),
            format!(
                "该组已有 {member_count} 名成员，超过上限 {MAX_GROUP_MEMBERS}；请先分批移出成员再修改组权限"
            ),
        ));
    }
    Ok(())
}

/// 当前处于启用的系统管理员人数（内置全权组的 active 成员）。
///
/// 成员数从组事实读取；active 判定复用 `AuthorizationPort` 的版本快照
/// （`users.status` 的事实源），避免在此另写一条用户状态查询路径。
pub(crate) async fn count_active_system_admins_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
) -> Result<u64, BaseError> {
    let Some(group) = access
        .groups()
        .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
        .await?
    else {
        return Ok(0);
    };
    let members = access
        .groups()
        .list_members_in_tx(ctx, transaction, group.id)
        .await?;
    let mut active = 0_u64;
    for user_id in members {
        if let Some(snapshot) = access
            .authorization()
            .find_authorization_version(ctx.tools().mysql()?.pool(), user_id)
            .await?
        {
            if snapshot.is_active() {
                active += 1;
            }
        }
    }
    Ok(active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn self_escalation_is_rejected_when_new_permission_appears() {
        let before = set(&["access.groups.write"]);
        let after = set(&["access.groups.write", "access.grants.write"]);
        let error = match assert_no_self_escalation(&before, &after) {
            Ok(()) => panic!("提权必须被拒"),
            Err(error) => error,
        };
        assert!(
            matches!(error, yang_base::BaseError::PermissionDenied(_)),
            "应为 403，实际 {error:?}"
        );
    }

    #[test]
    fn subset_change_is_allowed() {
        let before = set(&["access.groups.write", "access.grants.read"]);
        let after = set(&["access.groups.write"]);
        assert!(assert_no_self_escalation(&before, &after).is_ok());
    }

    #[test]
    fn identical_sets_are_allowed() {
        let before = set(&["access.groups.write"]);
        assert!(assert_no_self_escalation(&before, &before.clone()).is_ok());
    }

    #[test]
    fn escalation_error_names_only_the_newly_added_permissions() {
        // 报错信息是调用者判断「到底多出了什么」的唯一依据：它必须只列出
        // 新增权限（after - before），既不能漏报也不能把已有权限混进去。
        let before = set(&["access.groups.write"]);
        let after = set(&[
            "access.groups.write",
            "access.grants.write",
            "account.users.read",
        ]);
        let error = match assert_no_self_escalation(&before, &after) {
            Ok(()) => panic!("提权必须被拒"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("access.grants.write"),
            "必须点名新增权限，实际 {message}"
        );
        assert!(
            message.contains("account.users.read"),
            "必须点名全部新增权限，实际 {message}"
        );
        assert!(
            !message.contains("access.groups.write"),
            "已有权限不应出现在提权报告里，实际 {message}"
        );
    }

    #[test]
    fn member_limit_boundary_is_exactly_200() {
        // Review Focus 4：200 与 201 的行为必须不同。
        assert!(ensure_member_limit(0).is_ok());
        assert!(ensure_member_limit(199).is_ok());
        assert!(ensure_member_limit(200).is_ok(), "等于上限必须允许");
        let error = match ensure_member_limit(201) {
            Ok(()) => panic!("超过上限必须拒绝"),
            Err(error) => error,
        };
        assert!(
            matches!(error, yang_base::BaseError::ParamInvalid(_, _)),
            "超限必须是调用方按提示可纠正的拒绝，实际 {error:?}"
        );
        assert!(error.to_string().contains("200"), "错误信息必须给出上限");
        assert!(
            error.to_string().contains("201"),
            "错误信息必须给出实际成员数，实际 {error}"
        );
    }
}
