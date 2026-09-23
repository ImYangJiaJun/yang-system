//! 向权限组追加一条权限（幂等）。

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

    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
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
        // 上限先于写入：超限时不得进入 O(N) 行锁事务（spec §6.3 与 Review Focus 4）。
        let members = access
            .groups()
            .list_members_in_tx(&ctx, &mut transaction, group.id)
            .await?;
        ensure_member_limit(members.len() as u64)?;

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
        // 组权限是成员有效权限的来源，变更后必须让全部成员的 Token 失效
        // （锁序由 list_members_in_tx 的升序保证）。
        let affected: BTreeSet<i64> = members.into_iter().collect();
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
