//! 把一个用户移出权限组（幂等），并守住 spec §8.2 的最后管理员守卫。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{
    count_active_system_admins_in_tx, invalidate_users_in_tx,
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
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;

        // spec §8.2：移出全权组成员之后，系统必须仍有至少一名 active 管理员。
        //
        // 判定必须建立在「目标本人是否已计入这份计数」上：移出一名**已停用**的
        // 管理员成员不减少启用管理员数，裸计数（`members.contains && admins <= 1`）
        // 会把这种合法操作误拒成最后管理员守卫。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            let admins =
                count_active_system_admins_in_tx(&access, &ctx, &mut transaction, input.user_id)
                    .await?;
            if !admins.keeps_at_least_one_admin() {
                return Err(last_admin_guard());
            }
        }
        // 这里**刻意不做** §8.1 的自提权校验：移出只会减少权限，永远不会让调用者的
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
