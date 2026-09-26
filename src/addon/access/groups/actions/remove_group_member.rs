//! 把一个用户移出权限组（幂等），守住 spec §8.1 的「只有全权组成员能修改全权组成员」
//! 与 §8.2 的最后管理员守卫——前者**必须**先于后者判定，否则非管理员能借「还剩 >=2 名
//! 启用管理员」反复移出管理员，直到只剩他指定的那一名。
//!
//! 判定所依据的成员名单必须在**持有目标组组行锁**的前提下读到（`lock_group_in_tx`）：
//! 移出成员改的是成员行而不是 `users.status`，非锁定的成员读会停在陈旧快照上，两个并发
//! 移出各自读到同一份名单、各自数出「还有两名启用管理员」而双双放行，把 `system_admin`
//! 组清零。锁序与探查（定锁集）的取舍见 `domain::groups::admin` 的模块注释。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{
    count_active_system_admins_of_members_in_tx, invalidate_users_in_tx, lock_users_ascending_in_tx,
};
use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RemoveGroupMemberInput {
        group_id: Key::new()
            .title("权限组")
            .require(true),
        user_id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct RemoveGroupMemberResult {
    group_id: i64,
    user_id: i64,
    /// 本次是否真的删除了成员关系：该用户本就不在此组时为 `false`，既不递增任何人
    /// 的授权版本，也不写审计事件（spec §9.2 的幂等语义）。
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RemoveGroupMemberInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();

    // 事务之外只读探查，**只**用来确定下一个事务的 users 行锁集合：
    //
    // - 先判本次是否针对**内置全权组**（按 `group_key` 查它的 id；`group_key` 不可变、
    //   该组引导后恒定存在，因此这个判断稳定）；
    // - 只有内置组需要把**全体成员**纳入锁批：它承载「至少留一名启用管理员」的不变量，
    //   守卫要按升序锁住每个成员的 users 行才能拿到当前状态、并挡住并发停用。普通组的
    //   成员集合没有对应不变量，锁批只需 `{操作者, 目标}`，不为它扩大锁范围。
    //
    // 探查放在事务之外，就不会在正式事务里建立早于行锁的一致性读快照（与 `add_group_item`
    // 同一手法、同一理由）。它用来定锁集，因此可以陈旧：少算一行只是少锁一行（由下面
    // `countable` 交集把少算折算成 fail-closed 的少计），多算一行只是多重锁一行。
    // 判据绝不取这份数据——守卫读的是组行锁之后的那份名单。
    let probed_members = {
        let mut probe = ctx.tools().mysql()?.transaction().await?;
        let admin_group_id = access
            .groups()
            .find_by_key_in_tx(&ctx, &mut probe, SYSTEM_ADMIN_GROUP_KEY)
            .await?
            .map(|group| group.id);
        let members = if admin_group_id == Some(input.group_id) {
            access
                .groups()
                .list_members_in_tx(&ctx, &mut probe, input.group_id)
                .await?
        } else {
            Vec::new()
        };
        Access::finish_transaction(probe, Ok(())).await?;
        members
    };

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 第一把锁：users 行。按 `user_id` 升序一次性锁住「探查到的全权组成员 ∪ 操作者 ∪
        // 目标」。
        //
        // 锁序：必须整集升序（理由见 `lock_users_ascending_in_tx`），且必须**先于**组行锁
        // （全局锁序见 `domain::groups::admin` 的模块注释）：`add_group_item` 是先锁
        // users 行、再由外键取父组行的 S 锁，成员变更也必须同向，否则在同一组行上成环。
        let mut lock_set: BTreeSet<i64> = probed_members.iter().copied().collect();
        lock_set.insert(operator_id);
        lock_set.insert(input.user_id);
        lock_users_ascending_in_tx(&access, &ctx, &mut transaction, &lock_set).await?;

        // 第二把锁：**目标组的组行**（`FOR UPDATE`）。它是成员变更的串行化点，与
        // `add_group_member` 完全对称：同一组的成员变更在此排队，后到者读到的成员名单
        // 必然包含先到者的提交，两个并发移出因此不再能各自停在陈旧名单上双双放行。
        let group = access
            .groups()
            .lock_group_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;

        // 内置全权组的两条守卫都必须建立在同一份名单上。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            // 组行锁之后的第一批一致性读：Read View 在此建立，必然晚于两把行锁；持锁期间
            // 同一组的成员变更被组行锁挡住，因此这份名单在提交前不会变。
            let members = access
                .groups()
                .list_members_in_tx(&ctx, &mut transaction, group.id)
                .await?;
            // spec §8.1 附加规则：只有全权组成员能修改全权组成员——移出路径与
            // `add_group_member` 完全对称（同一个判定、同一句话）。
            //
            // **必须排在下面 §8.2 的最后管理员判定之前**：那条判定只在「移出后一个人都不剩」
            // 时才拒绝，因此它对「组里还剩 >=2 名启用管理员」的移除一律放行。少了本守卫，
            // 持有 `access.groups.write` 的非管理员就能把管理员逐个移出——每次都能通过最后
            // 管理员判定，反复执行直到组内只剩他指定的那一名，把守卫本身变成摆设。
            if !members.contains(&operator_id) {
                return Err(BaseError::PermissionDenied(
                    "只有系统管理员可以修改系统管理员组的成员".to_string(),
                ));
            }

            // spec §8.2：移出全权组成员之后，系统必须仍有至少一名 active 管理员。
            //
            // 判定必须建立在「目标本人是否已计入这份计数」上：移出一名**已停用**的
            // 管理员成员不减少启用管理员数，裸计数（`members.contains && admins <= 1`）
            // 会把这种合法操作误拒成最后管理员守卫。
            //
            // 计数名单取「本次已持锁成员」与「组行锁后读到的成员」的**交集**：探针与组行锁
            // 之间若新入组了成员，它不在 users 行锁批里，此刻再对它取锁会破坏升序、可能
            // 与并发加成员成环（对方持它的 users 行、等本事务已持有的组行）。少计这种
            // 成员只会让守卫更保守（fail-closed 方向），不会放宽「至少留一名启用管理员」。
            let countable: Vec<i64> = members
                .iter()
                .copied()
                .filter(|user_id| lock_set.contains(user_id))
                .collect();
            let admins = count_active_system_admins_of_members_in_tx(
                &access,
                &ctx,
                &mut transaction,
                &countable,
                input.user_id,
            )
            .await?;
            if !admins.keeps_at_least_one_admin() {
                return Err(last_admin_guard());
            }
        }
        // 这里**刻意不做** §8.1 的自提权（权限子集）校验：移出只会减少权限，永远不会让调用者的
        // 有效权限变大，而管理员必须能退出全权组（否则最后一个想走的管理员被锁死）。

        let changed = access
            .groups()
            .delete_member_in_tx(&ctx, &mut transaction, input.user_id, group.id)
            .await?;
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写审计。
        }
        // 失去组权限后该用户的旧 Token 必须立即失效（spec §6.3）。
        let mut affected = BTreeSet::new();
        affected.insert(input.user_id);
        invalidate_users_in_tx(&access, &ctx, &mut transaction, &affected).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group.id)?,
            None,
            Some(audit::summary([("user_id", json!(input.user_id))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(true)
    }
    .await;
    let changed = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        RemoveGroupMemberResult {
            group_id: input.group_id,
            user_id: input.user_id,
            changed,
        },
        if changed {
            "成员已移出权限组，其刷新会话后失去组内权限"
        } else {
            "该用户本就不在此权限组中"
        },
    )
}

/// 「最后一名管理员」的统一拒绝。
///
/// 计划与设计 §9.3 此处写的是 `BaseError::Conflict`（409）：`yang_base::BaseError`
/// 并没有该变体，且设计同节要求「不为个别用例扩展框架错误类型」，因此沿用既有的
/// `ParamInvalid`——与 `ensure_member_limit`、`delete_group` 的 `group_has_members`
/// 对同类「资源状态冲突」的取舍一致，消息里给出可执行的处置办法。
fn last_admin_guard() -> BaseError {
    BaseError::ParamInvalid(
        "user_id".to_string(),
        "该用户是最后一名启用的系统管理员，移出后系统将无人可管理权限".to_string(),
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("remove_group_member"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/members/remove")
        .display_name("移出组成员")
        .description("把一个用户移出权限组（幂等；最后一个系统管理员不可移出）")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_and_target_user_contract() {
        let injected = serde_json::from_value::<RemoveGroupMemberInput>(serde_json::json!({
            "group_id": 3,
            "user_id": 7,
            "force": true
        }));
        assert!(injected.is_err(), "客户端不能注入 force 等内部字段");

        let without_group =
            serde_json::from_value::<RemoveGroupMemberInput>(serde_json::json!({ "user_id": 7 }));
        assert!(without_group.is_err(), "缺少 group_id 必须被拒绝");

        let params = <RemoveGroupMemberInput as ParamInput>::params();
        let names: Vec<&str> = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        assert_eq!(names, ["group_id", "user_id"]);
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn last_admin_guard_is_a_readable_client_error() {
        let error = last_admin_guard();
        assert_eq!(
            error.code(),
            BaseError::ParamInvalid("user_id".to_string(), String::new()).code(),
            "必须是既有的 ParamInvalid 错误码（框架没有 Conflict 变体）"
        );
        let message = error.to_string();
        assert!(
            message.contains("系统管理员"),
            "必须说明撞上的是哪条守卫，实际 {message}"
        );
        assert!(
            !matches!(error.category(), yang_base::error::ErrorCategory::Server),
            "守卫拒绝不是服务端故障，绝不能映射成 5xx"
        );
    }
}
