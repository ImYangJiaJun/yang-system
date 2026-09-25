//! 向权限组追加一条权限（幂等）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{
    assert_no_self_escalation, effective_permissions_of_in_tx, ensure_may_grant_permission_in_tx,
    ensure_member_limit, invalidate_users_in_tx, lock_users_ascending_in_tx,
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
    pub(super) AddGroupItemInput {
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
pub(super) struct AddGroupItemResult {
    group_id: i64,
    permission: String,
    /// 本次是否真的新增了条目：重复添加同一权限时为 `false`，既不递增任何人的
    /// 授权版本，也不写授权 Outbox（spec §9.2 的幂等语义）。
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AddGroupItemInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    // 只能加入 Catalog 中已声明的权限（fail-closed，沿用 grant_permission 的语义）。
    // 该校验先于事务：未声明的权限根本不该走到「锁住全体成员」那一步。
    access
        .permission_catalog()
        .ensure_declared(&input.permission)?;

    // 第一步（在事务之外）：只读探查一次组成员，**只**用来确定下一个事务的行锁集合。
    //
    // 为什么不能直接在本事务里先读成员、再按升序加锁：InnoDB 可重复读下，事务快照
    // （Read View）由事务里**第一条普通 SELECT** 建立，而锁定读不建立它。一旦判据读
    // 排在加锁之前，后续所有读都停在那个旧快照上，看不见并发事务的提交——锁等于没加
    // （实测：并发自提权用例依旧红）。而「按升序一次锁住 {操作者} ∪ {成员}」又必须先
    // 知道成员集合。于是把这次「只为定锁集」的探查放到事务之外：它自己那个只读事务
    // 结束时不留任何锁，也不影响下一个事务的快照。
    //
    // 探查只用来定锁集，因此它可以陈旧：锁集少算了一行只是少锁一行（扇出也不会去碰
    // 锁批之外的行，见下方），多算了一行只是多重锁一行。判据本身绝不取这份数据。
    let probed_members = {
        let mut probe = ctx.tools().mysql()?.transaction().await?;
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut probe, input.group_id)
            .await?;
        Access::finish_transaction(probe, Ok(())).await?;
        members
    };
    // 上限先于行锁：超限时不得进入 O(N) 行锁事务（spec §6.3 与 Review Focus 4）。
    // 用探查到的成员数判定，正是为了在加锁**之前**就拒绝。
    ensure_member_limit(probed_members.len() as u64)?;

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 第二步：本事务的**第一条语句**就是这批行锁——按 `user_id` 升序一次性锁住
        // {操作者} ∪ {组成员}。两个性质缺一不可：
        //
        // 1. **串行化**：`add_group_item` 与 `add_group_member` 的判据都是普通 SELECT，
        //    同一账号并发发起两者时各自读到对方未提交的快照、各自通过 §8.1 子集校验，
        //    提交后调用者就成了「持有一条自己原本没有的权限」的组成员。操作者行在
        //    这批锁里，两条路径因此在同一行上排队，后到者必定在先到者提交之后才读判据。
        // 2. **锁序**：升序是 `invalidate_users_in_tx` 扇出与
        //    `count_active_system_admins_in_tx` 共同遵守的全局唯一锁序（理由与反例见
        //    `lock_users_ascending_in_tx`）。写成「先单独锁操作者行、再升序锁成员行」
        //    会让同一组的两名成员操作者各持自己的行、再去要对方那一行，直接成环。
        let mut lock_set: BTreeSet<i64> = probed_members.into_iter().collect();
        lock_set.insert(operator_id);
        lock_users_ascending_in_tx(&access, &ctx, &mut transaction, &lock_set).await?;

        // 第三步：锁后读。它是本事务的第一条普通读，因此快照在此建立——必然包含
        // 「先于本事务取到这批锁」的全部提交，是判据的唯一事实来源。
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
        // 内置全权组的权限由权限目录计算，写入条目既无意义又会让解析出现第二口径。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            return Err(BaseError::ParamInvalid(
                "group_id".to_string(),
                "内置系统管理员组的权限由权限目录计算，不能增删条目".to_string(),
            ));
        }
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        // 上限在加锁前已判定过一次；这里再按锁后快照复核一次，避免探查与加锁之间
        // 有成员加入把组顶到上限之外。
        ensure_member_limit(members.len() as u64)?;

        // G2 闸门：把管理员等价权限加进组同样是「授予」，只能由全权组成员执行。
        // 本次新增的就是 `input.permission`（已在上方 `ensure_declared` 确认属于目录），
        // 因此直接按它判定。这条与下面的 §8.1 提权校验彼此独立：这条问「你有没有资格
        // 授予」，那条问「这次操作会不会让你自己变大」——非成员调用者不触发后者。
        ensure_may_grant_permission_in_tx(
            &access,
            &ctx,
            &mut transaction,
            operator_id,
            &input.permission,
        )
        .await?;

        // spec §8.1 路径二：给自己**已属于**的组加一条自己没有的权限，同样是自提权。
        // 组权限只影响成员，因此只有调用者本身在该组内时这条不变量才可能被破坏。
        // 权限已在上方经 `ensure_declared` 确认属于目录，因此新增项就是它本身。
        if members.contains(&operator_id) {
            let before =
                effective_permissions_of_in_tx(&access, &ctx, &mut transaction, operator_id)
                    .await?;
            let mut after = before.clone();
            after.insert(input.permission.clone());
            assert_no_self_escalation(&before, &after)?;
        }

        let changed = access
            .groups()
            .insert_item_in_tx(
                &ctx,
                &mut transaction,
                group.id,
                &input.permission,
                operator_id,
            )
            .await?;
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写 Outbox。
        }
        // 组权限是成员有效权限的来源，变更后必须让成员的 Token 失效。
        //
        // 扇出集合严格取「本次已按升序持锁的成员行」：`invalidate_users_in_tx` 只对锁批
        // 内的行重读锁值并递增版本，因此**整个事务**的用户行锁获取顺序始终非降序，
        // 不会有第二把乱序的锁落进来（这正是死锁环成立的必要条件，见
        // `lock_users_ascending_in_tx`）。
        //
        // 探查之后才被加入本组的成员不在此集合里，这是安全的：加入它的
        // `add_group_member` 自身已经递增过它的授权版本；而漏发一次「本可让它更早看到
        // 这条新权限」的失效只会让它暂时少一条权限（欠授权），不会多一条。
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
            None,
            Some(audit::summary([("permission", json!(input.permission))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(true)
    }
    .await;
    let changed = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        AddGroupItemResult {
            group_id: input.group_id,
            permission: input.permission,
            changed,
        },
        if changed {
            "权限已加入组，组成员刷新会话后生效"
        } else {
            "该组已持有该权限"
        },
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("add_group_item"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/items")
        .display_name("加入组权限")
        .description("向权限组追加一条已声明的权限（幂等）")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_id_and_permission_format_contract() {
        let injected = serde_json::from_value::<AddGroupItemInput>(serde_json::json!({
            "group_id": 3,
            "permission": "access.grants.read",
            "granted_by": 7
        }));
        assert!(injected.is_err(), "客户端不能注入 granted_by 等内部字段");

        let without_permission =
            serde_json::from_value::<AddGroupItemInput>(serde_json::json!({ "group_id": 3 }));
        assert!(without_permission.is_err(), "缺少 permission 必须被拒绝");

        let params = <AddGroupItemInput as ParamInput>::params();
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
        assert_eq!(
            param("permission").validation.max_length,
            Some(PERMISSION_MAX_LENGTH),
            "格式上限必须与条目表的列宽同源，否则会被数据库打回成 500"
        );
    }
}
