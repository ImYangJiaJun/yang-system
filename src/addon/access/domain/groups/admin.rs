//! 组管理的事务编排：扇出失效、提权校验、最后管理员判定与成员上限。
//!
//! 这些机制被多个 Action 共用，集中在此以保证语义单一——尤其
//! `effective_permissions_of_in_tx` 必须与 `GroupGrantResolver` 走同一
//! 解析函数（spec §8.1）。
//!
//! # 全仓统一的锁序
//!
//! 组事实的并发正确性只依赖三把锁。任何事务都必须按下面的**先后**取锁，同一层内再按
//! 既定顺序（users 行一律升序），环才无从闭合：
//!
//! 1. **users 行锁**——一律按 `user_id` 升序**整批**获取：成员变更开头的
//!    `lock_users_ascending_in_tx`、最后管理员判定里对每个成员的
//!    `lock_authorization_version`，以及 `invalidate_users_in_tx` 在同一事务内的复核
//!    递增。升序纪律的反例与证明见 `lock_users_ascending_in_tx`。
//! 2. **目标组行锁**——`repository::GroupRepository::lock_group_in_tx`（
//!    `permission_group` 主键上的 `FOR UPDATE` 记录锁）。它是成员变更的**串行化点**：
//!    `读成员名单 → 判据 → 写成员行` 必须整段串行化，否则两个并发移出会各自停在陈旧
//!    快照上、各自读到同一份名单而双双放行，`system_admin` 组被清零（spec §8.2 明称
//!    该状态不可达）。它必须出现在 users 行锁**之后**——见下方「为什么是 users 行 →
//!    组行」。
//! 3. **其余 DML 的隐式索引锁**——`user_group` 的 DELETE/INSERT、
//!    `permission_group_item`、`audit_event`、`authorization_outbox` 的索引锁只能
//!    落在最后。
//!
//! ## 为什么是「users 行 → 组行」，而不是反过来（收尾回合改正）
//!
//! 这条相对次序是被 `add_group_item` 逼出来的：它先按升序锁 users 行（`{操作者} ∪
//! {组成员}`），再在插入条目时由 `permission_group_item` 的外键取**父组行**的 S 锁，
//! 次序正是「users 行 → 组行」。成员变更若反过来先取组行 X 锁、再取 users 行，就会与
//! 它构成 ABBA 环：一方持组行等 users 行、另一方持 users 行等组行（父外键 S 锁），
//! MySQL 只能牺牲一个。因此成员变更统一为「先 users 行、后组行」。
//!
//! 这个顺序下 `remove_group_member` 在事务前先做一次只读**探查**确定 users 行锁集合
//! （成员 id 必须先于加锁知道）。探针允许陈旧：只为定锁集，判据读在组行锁之后。
//!
//! ## 关于「拿成员行当串行化点」（收尾回合实测推翻，勿回退）
//!
//! 曾经的实现用 `SELECT ... WHERE group_id = ? FOR UPDATE` 直接锁 `user_group` 的成员行。
//! 对**成员集合为空**的组，这条语句只会命中彼此兼容的**间隙锁**，压根没有串行化；两个
//! 并发加成员都通过它、都读到空名单，随后一方持 users 行等对方、另一方持间隙锁等对方的
//! 插入意向锁，死锁（真库 40001 → 500）。组行记录锁对空组同样互斥，才是可用的串行化点。
//!
//! ## 与账号生命周期守卫的交叉
//!
//! `disable_self` / `admin_disable_user` / `delete_account` 三条路径**先**用
//! `lock_credential_in_tx` 锁住目标 users 行，**再**调用
//! `count_active_system_admins_in_tx`（**非锁定**的成员读 + 对成员逐个锁定状态读）。
//! 它们与成员变更同向（都是「users 行 → …」），因此不必也不该再取组行锁——成员变更会
//! 先锁住该组**全部**成员的 users 行（`remove_group_member` 的探针即为此），而生命周期
//! 目标本身就是该组成员，所以那次成员变更一定排在守卫之后提交。守卫的成员名单即使是陈旧
//! 快照也安全：**少算**某个刚入组的成员只会更保守（fail-closed）；**多算**某个刚被移出的
//! 成员这一支被上述 users 行锁挡住。入组只锁 `{操作者, 目标}`，可能绕开守卫目标那一行，
//! 但入组只会让启用管理员变多。

// 消费者（组管理 Action 在 Task 10+、账号生命周期守卫在 Task 13）晚于本任务
// 接入，与 `repository.rs` / `resolution.rs` 同例显式豁免 dead-code 门禁。
#![allow(dead_code)]

use super::repository::{GroupRecord, MAX_GROUP_MEMBERS, SYSTEM_ADMIN_GROUP_KEY};
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

/// 在调用方事务内按 `user_id` **升序**锁定一批用户行（`FOR UPDATE`）。
///
/// **锁序契约**：这是「操作者行 + 组成员行」两类行锁的唯一入口。调用方必须
/// 一次性把本次事务将触碰的**全部**用户行交给它，并在**读取任何有效权限/成员/条目
/// 快照之前**调用——`add_group_item` 与 `add_group_member` 都遵守这条纪律。
///
/// 为什么是「整集升序」而不是「先锁操作者行、再锁成员行」：升序是
/// [`invalidate_users_in_tx`] 的扇出与 [`count_active_system_admins_in_tx`] 共同遵守的
/// 全局唯一锁序，只有所有事务都按同一顺序取行锁，环才无从闭合。而「操作者行优先」
/// 会让操作者成为序列里的**第一个**元素：当操作者本身也是组成员时，同一组的两名
/// 成员操作者会各自先锁自己、再去要对方的行（A 持 A 等 B，B 持 B 等 A），恰好构成
/// 一个死锁环——而按升序整集加锁时两个事务的取锁序列完全相同，环不成立。
/// 操作者行必在这批锁里，因此「同一账号的两个并发请求被这一行串行化」仍然成立：
/// 后到者一定在先到者提交之后才读到后续快照。
///
/// 只借这批行锁做互斥，不在此处递增版本：`invalidate_users_in_tx` 会在同一事务内
/// 依同一升序重新读取锁值并递增，版本语义（单调递增 + Outbox）仍只有一处实现。
pub(crate) async fn lock_users_ascending_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    user_ids: &BTreeSet<i64>,
) -> Result<(), BaseError> {
    for user_id in user_ids {
        // `BTreeSet` 的迭代顺序即升序，不要改成 `HashSet`。
        let _held = access
            .authorization()
            .lock_authorization_version(ctx.tools().mysql()?.pool(), transaction, *user_id)
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

/// 模拟「本次入组之后」某用户的有效权限：把目标组对他的贡献并入当前有效权限。
///
/// **不写库**、只做集合运算。它存在的意义是让 [`assert_no_self_escalation`]
/// 有第二个可比快照，而那个快照必须与 `effective_permissions_of_in_tx` 出自
/// 同一条解析路径（spec §8.1），否则校验会与实际 Token 解析漂移成两套语义。
pub(crate) async fn simulate_after_join(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    user_id: i64,
    group: &GroupRecord,
) -> Result<BTreeSet<String>, BaseError> {
    let catalog = catalog_permissions(access.permission_catalog())?;
    let mut after = effective_permissions_of_in_tx(access, ctx, transaction, user_id).await?;
    let items = access
        .groups()
        .list_items_in_tx(ctx, transaction, group.id)
        .await?;
    after.extend(resolve_group_permissions(
        &group.group_key,
        &items,
        &catalog,
    ));
    Ok(after)
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
///
/// **封死的范围（如实记录，不用「理论上不会并发」代替）**：`add_group_member` 是唯一
/// 能经 API 增加成员的路径，它把上限判定放在目标组的**组行锁**之内（
/// `lock_group_in_tx`），成员数因此不会漂移，并发加不同成员也只能有一个
/// 成功——200 这条线对 API 路径是硬线。超出该锁保护范围、因而不被本函数拦住的两条
/// 写入是：**直接写库**（运维 SQL、集成测试夹具）与**引导声明**
/// （`SystemOwnerClaimer::claim` 经 `insert_member_in_tx` 写入内置全权组）。二者都各自
/// 只写入固定数量的行——引导由 `system_owner` 哨兵的唯一键保证至多一次——不是可被请求
/// 反复放大的路径，因此这里不为它们引入第二条计数口径。
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

/// 事务内加锁统计出的系统管理员启用情况。
///
/// 只返回一个总数不足以支撑守卫：调用方真正要回答的是「去掉**目标本人**之后
/// 是否还剩人」。目标若本来就不计入（不在组内，或自身已停用），那么任何针对他的
/// 操作都不会减少启用管理员数，此时即便系统只剩一名启用管理员也必须放行——
/// 这正是裸计数会误拒「移出一名已停用的管理员成员」这类合法操作的根因。
/// spec §8.2 的守卫语义是「本次操作之后仍有至少一名启用管理员」，
/// 而不是「当前至少有两名」。
pub(crate) struct ActiveSystemAdminCount {
    group_exists: bool,
    total: u64,
    target_included: bool,
}

impl ActiveSystemAdminCount {
    /// 内置全权组是否存在于库中。
    pub(crate) fn group_exists(&self) -> bool {
        self.group_exists
    }

    /// 当前处于启用状态的管理员总数。
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// 目标用户是否「在组内且自身启用」——即他是否已被计入 [`Self::total`]。
    pub(crate) fn target_included(&self) -> bool {
        self.target_included
    }

    /// 目标被停用 / 删除 / 移出全权组之后，系统是否仍有至少一名启用管理员。
    ///
    /// 目标未被计入时恒为真；被计入时需要 `total >= 2`（去掉他自己还剩一个）。
    pub(crate) fn keeps_at_least_one_admin(&self) -> bool {
        !self.target_included || self.total >= 2
    }
}

/// 当前处于启用的系统管理员情况（内置全权组的成员）。
///
/// **必须名副其实**：成员行与 `users.status` 都在**调用方事务内**读取——成员按
/// `user_id` 升序逐个 `lock_authorization_version`（`FOR UPDATE`）锁定后，再读锁
/// 句柄的 `is_active()` 判启用状态。绝不能再经连接池在事务外读状态：那样两个并发
/// 「停用/删除/移出管理员」会各自读到同一份未加锁的旧计数而双双放行，
/// `system_admin` 组被清零，而 spec §8.2 声称该状态不可达。
///
/// **锁序**：升序与 [`invalidate_users_in_tx`] 一致，是两个并发变更不互相死锁的
/// 唯一纪律（调用方给出的成员名单保证按 `user_id` 升序，不要在此重排）。
///
/// 成员行指向的 user 已不存在（并发删号窗口）时按「不计入」降级而不是整单硬失败，
/// 与解析侧 `group_resolver` 对悬空成员的口径一致。
///
/// 本函数只负责「读成员名单之外的一切」：名单由调用方传入，**必须**是它在本事务内
/// 持有目标组组行锁之后读到的那一份（`lock_group_in_tx` + `list_members_in_tx` 的结果），
/// 且名单里的每个 id 都已在同一事务内按升序持锁（否则这里会补一把乱序的 users 行锁）。
pub(crate) async fn count_active_system_admins_of_members_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    members: &[i64],
    target_user_id: i64,
) -> Result<ActiveSystemAdminCount, BaseError> {
    let mut total = 0_u64;
    let mut target_included = false;
    for user_id in members {
        let locked = match access
            .authorization()
            .lock_authorization_version(ctx.tools().mysql()?.pool(), transaction, *user_id)
            .await
        {
            Ok(locked) => locked,
            // 并发删号窗口：成员行还在、user 行已经没了。按「不计入」降级，
            // 不因为一行悬空成员就让整单失败。
            Err(BaseError::UserNotFound(_)) => continue,
            Err(error) => return Err(error),
        };
        if !locked.is_active() {
            continue;
        }
        total += 1;
        if *user_id == target_user_id {
            target_included = true;
        }
    }
    // 能走到这里说明内置全权组存在（成员名单是从它身上读出来的）。
    Ok(ActiveSystemAdminCount {
        group_exists: true,
        total,
        target_included,
    })
}

/// 账号生命周期路径（`disable_self` / `admin_disable_user` / `delete_account`）的守卫入口。
///
/// **刻意使用非锁定的成员读**：三个调用方都先 `lock_credential_in_tx` 锁住目标
/// users 行再进来，与成员变更路径同向（都是「users 行 → …」），因此这里不必也不该
/// 再取组行锁（完整论证见本模块顶部「全仓统一的锁序」）。它们的成员名单因此可能是
/// 陈旧快照，但守卫仍然成立：任何针对全权组的成员变更都先按升序锁住该组**全部**成员的
/// users 行，生命周期目标本身是成员，于是成员变更一定被挡在守卫之后。
///
/// 成员变更路径（`add_group_member` / `remove_group_member`）改用
/// [`count_active_system_admins_of_members_in_tx`]，并传入它在组行锁之后读到、
/// 且已按升序持锁的成员名单——那样成员名单与判据在同一把锁下，不再依赖上面这条推理。
pub(crate) async fn count_active_system_admins_in_tx(
    access: &Access,
    ctx: &ActionContext,
    transaction: &mut Transaction,
    target_user_id: i64,
) -> Result<ActiveSystemAdminCount, BaseError> {
    let Some(group) = access
        .groups()
        .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
        .await?
    else {
        return Ok(ActiveSystemAdminCount {
            group_exists: false,
            total: 0,
            target_included: false,
        });
    };
    let members = access
        .groups()
        .list_members_in_tx(ctx, transaction, group.id)
        .await?;
    count_active_system_admins_of_members_in_tx(access, ctx, transaction, &members, target_user_id)
        .await
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

    /// 构造一份「组存在」的计数结果，供守卫语义单测使用。
    fn count(total: u64, target_included: bool) -> ActiveSystemAdminCount {
        ActiveSystemAdminCount {
            group_exists: true,
            total,
            target_included,
        }
    }

    #[test]
    fn removing_the_target_still_leaves_an_admin_only_when_another_one_is_counted() {
        // 目标已计入（在组内且自身启用）：必须还有第二个人才算「操作后仍有人」。
        assert!(count(2, true).keeps_at_least_one_admin());
        assert!(count(3, true).keeps_at_least_one_admin());
        assert!(!count(1, true).keeps_at_least_one_admin());

        // 目标未计入（不在组内，或自身已停用）：任何针对他的操作都不减少启用
        // 管理员数，即便系统只剩一名启用管理员也必须放行——裸计数误拒的正是这一支。
        assert!(
            count(1, false).keeps_at_least_one_admin(),
            "移出一名已停用的管理员成员不得被误拒"
        );
        assert!(count(0, false).keeps_at_least_one_admin());
    }

    #[test]
    fn the_count_result_carries_both_the_total_and_the_target_state() {
        // 守卫需要「总数」与「目标是否已计入」两个事实同时可读：少任何一个，
        // 调用方都只能靠猜，而猜错的方向就是把合法操作误拒或把不安全操作放行。
        let snapshot = count(3, true);
        assert_eq!(snapshot.total(), 3);
        assert!(snapshot.target_included());
        assert!(snapshot.group_exists());

        // 组不存在时三个事实都必须给出确定值，不能靠调用方自行推断。
        let missing = ActiveSystemAdminCount {
            group_exists: false,
            total: 0,
            target_included: false,
        };
        assert!(!missing.group_exists());
        assert_eq!(missing.total(), 0);
        assert!(!missing.target_included());
    }
}
