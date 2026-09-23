//! 把一个用户加入权限组（幂等），并守住 spec §8.1 的防自提权不变量。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{
    assert_no_self_escalation, effective_permissions_of_in_tx, invalidate_users_in_tx,
    simulate_after_join,
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
    pub(super) AddGroupMemberInput {
        group_id: Key::new()
            .title("权限组")
            .require(true),
        user_id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AddGroupMemberResult {
    group_id: i64,
    user_id: i64,
    /// 本次是否真的新增了成员关系：重复添加同一成员时为 `false`，既不递增任何人的
    /// 授权版本，也不写审计事件（spec §9.2 的幂等语义）。
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: AddGroupMemberInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    // 目标用户必须先存在：外键只能告诉我们「这次插入失败了」，分不出是用户不存在
    // 还是组被并发删掉。先读一次版本快照，剩下的失败原因就只有后者（见下方兜底）。
    if access
        .authorization()
        .find_authorization_version(ctx.tools().mysql()?.pool(), input.user_id)
        .await?
        .is_none()
    {
        return Err(BaseError::UserNotFound(format!("用户 {}", input.user_id)));
    }

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;

        // spec §8.1 附加规则：只有全权组成员能修改全权组成员。防的是「刚被移出全权组
        // 的账户立刻把自己（或同伙）加回去」——这条防线不依赖自提权判定。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            let admins = access
                .groups()
                .list_members_in_tx(&ctx, &mut transaction, group.id)
                .await?;
            if !admins.contains(&operator_id) {
                return Err(BaseError::PermissionDenied(
                    "只有系统管理员可以修改系统管理员组的成员".to_string(),
                ));
            }
        }

        // spec §8.1 主不变量：任何操作都不得使调用者自身有效权限增大。
        // 只有「目标就是调用者自己」时才存在提权可能；修改他人是正常的授权管理行为。
        if input.user_id == operator_id {
            let before =
                effective_permissions_of_in_tx(&access, &ctx, &mut transaction, operator_id)
                    .await?;
            let after =
                simulate_after_join(&access, &ctx, &mut transaction, operator_id, &group).await?;
            assert_no_self_escalation(&before, &after)?;
        }

        let changed = match access
            .groups()
            .insert_member_in_tx(&ctx, &mut transaction, input.user_id, group.id, operator_id)
            .await
        {
            Ok(changed) => changed,
            // 外键 RESTRICT 的兜底窗口：组在本事务读到它之后被并发删除。用户存在性
            // 已在上方确认，因此这里只可能是组消失，折算成与前置读取同为 404 的
            // 拒绝，绝不能把 500 泄漏给客户端（与 delete_group 的兜底同例）。
            Err(error) if is_referential_constraint(&error) => {
                return Err(BaseError::RecordNotFound("权限组".to_string()));
            }
            Err(error) => return Err(error),
        };
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写审计。
        }
        // 新成员的有效权限变了，其已签发的 Access Token 必须立即失效（spec §6.3）。
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
        AddGroupMemberResult {
            group_id: input.group_id,
            user_id: input.user_id,
            changed,
        },
        if changed {
            "成员已加入权限组，其刷新会话后生效"
        } else {
            "该用户已在此权限组中"
        },
    )
}

/// 该错误是否表示「外键约束拒绝了这次成员写入」。
///
/// 与 `delete_group.rs` 的同名判定同义：`user_group` 上的外键在 INSERT 时只有
/// 「父行不存在」一种违反方式，而用户存在性已在事务外确认过，因此认出约束类错误
/// 就足够，不必解析 MySQL 的报文案（跨库、跨版本都不稳定）。
fn is_referential_constraint(error: &BaseError) -> bool {
    matches!(
        error,
        BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("add_group_member"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/members")
        .display_name("加入组成员")
        .description("把一个用户加入权限组（幂等；受防自提权子集校验约束）")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_and_target_user_contract() {
        let injected = serde_json::from_value::<AddGroupMemberInput>(serde_json::json!({
            "group_id": 3,
            "user_id": 7,
            "granted_by": 1
        }));
        assert!(injected.is_err(), "客户端不能注入 granted_by 等内部字段");

        let without_user =
            serde_json::from_value::<AddGroupMemberInput>(serde_json::json!({ "group_id": 3 }));
        assert!(without_user.is_err(), "缺少 user_id 必须被拒绝");

        let params = <AddGroupMemberInput as ParamInput>::params();
        let names: Vec<&str> = params
            .as_slice()
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["group_id", "user_id"],
            "参数集必须恰好是「哪个组 + 哪个用户」，多一个就是越权入口"
        );
        assert!(params.as_slice().iter().all(|param| param.required));
    }

    #[test]
    fn only_constraint_failures_are_treated_as_a_vanished_group() {
        // 认错变体会把真实故障吞成 404，或把并发删组泄漏成 500。
        assert!(is_referential_constraint(
            &BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(
                "Cannot add or update a child row: a foreign key constraint fails".to_string()
            ))
        ));
        assert!(!is_referential_constraint(
            &BaseError::DatabaseExecuteFailed(yang_db::DbError::Unknown("连接被重置".to_string()))
        ));
        assert!(!is_referential_constraint(&BaseError::RecordNotFound(
            "权限组".to_string()
        )));
    }
}
