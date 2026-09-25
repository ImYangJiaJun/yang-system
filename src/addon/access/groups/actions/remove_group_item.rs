//! 从权限组移除一条权限（幂等）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::admin::{ensure_member_limit, invalidate_users_in_tx};
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

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let group = access
            .groups()
            .find_by_id_in_tx(&ctx, &mut transaction, input.group_id)
            .await?
            .ok_or_else(|| BaseError::RecordNotFound("权限组".to_string()))?;
        // 内置全权组的权限由权限目录计算，条目表里根本没有可删的事实。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            return Err(BaseError::ParamInvalid(
                "group_id".to_string(),
                "内置系统管理员组的权限由权限目录计算，不能增删条目".to_string(),
            ));
        }
        // 与加条目对称：超限时同样拒绝，不得进入 O(N) 行锁事务（spec §6.3）。
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        ensure_member_limit(members.len() as u64)?;

        let changed = access
            .groups()
            .delete_item_in_tx(&ctx, &mut transaction, group.id, &input.permission)
            .await?;
        if !changed {
            return Ok(false); // 幂等：不递增版本、不写 Outbox。
        }
        let affected: BTreeSet<i64> = members.into_iter().collect();
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

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("remove_group_item"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/items/remove")
        .display_name("移除组权限")
        .description("从权限组移除一条权限（幂等；已不在目录中的孤儿条目同样可清理）")
        .permissions(["access.groups.write"])
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
