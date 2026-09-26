//! 改写权限组的展示字段（`title` / `description`）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::groups::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) UpdateGroupInput {
        group_id: Key::new()
            .title("权限组")
            .require(true),
        title: Str::new()
            .title("展示名")
            .require(true)
            .min_length(1)
            .max_length(128),
        description: Str::new()
            .title("描述")
            .require(false)
            .max_length(255),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct UpdateGroupResult {
    id: i64,
    title: String,
    description: Option<String>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: UpdateGroupInput,
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
        // 内置组的展示信息由引导流程写定，改名只会让「系统管理员」这个名字漂移。
        if group.group_key == SYSTEM_ADMIN_GROUP_KEY {
            return Err(BaseError::ParamInvalid(
                "group_id".to_string(),
                "内置系统管理员组不可修改展示信息".to_string(),
            ));
        }
        let affected = access
            .groups()
            .update_group_in_tx(
                &ctx,
                &mut transaction,
                group.id,
                &input.title,
                input.description.as_deref(),
            )
            .await?;
        if affected == 0 {
            return Err(BaseError::RecordNotFound("权限组".to_string()));
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group.id)?,
            Some(audit::summary([
                ("title", json!(group.title)),
                ("description", json!(group.description)),
            ])?),
            Some(audit::summary([
                ("title", json!(input.title)),
                ("description", json!(input.description)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(())
    }
    .await;
    // `title`/`description` 不是授权事实：本次改动不改变任何人的有效权限，
    // 因此**不**做扇出失效，否则每次改名都会让全组成员的下一个请求被迫重新登录。
    Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        UpdateGroupResult {
            id: input.group_id,
            title: input.title,
            description: input.description,
        },
        "权限组已更新",
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("update_group"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups/update")
        .display_name("修改权限组")
        .description("修改权限组的展示名与描述（组标识不可改）")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_cannot_carry_a_group_key_or_a_missing_group_id() {
        // `group_key` 是组的身份，改名会让已有审计与解析口径对不上，输入里根本
        // 不该有这个字段——注入它必须被拒绝，而不是被静默忽略。
        let with_key = serde_json::from_value::<UpdateGroupInput>(serde_json::json!({
            "group_id": 3,
            "title": "运维",
            "group_key": "ops2"
        }));
        assert!(with_key.is_err(), "组标识不可通过修改接口变更");

        let without_id = serde_json::from_value::<UpdateGroupInput>(serde_json::json!({
            "title": "运维"
        }));
        assert!(without_id.is_err());

        let params = <UpdateGroupInput as ParamInput>::params();
        let group_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "group_id")
            .unwrap_or_else(|| panic!("应声明 group_id 参数"));
        assert!(group_id.required);
        assert!(
            !params
                .as_slice()
                .iter()
                .any(|param| param.name.as_str() == "group_key"),
            "修改接口不得声明 group_key 参数"
        );
    }
}
