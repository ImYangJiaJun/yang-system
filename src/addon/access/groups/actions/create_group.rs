//! 创建一个权限组。

use crate::addon::access::domain::context::Access;
use crate::addon::access::groups::table::{GROUP_KEY_MAX_LENGTH, GROUP_KEY_PATTERN};
use crate::audit;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) CreateGroupInput {
        group_key: Str::new()
            .title("组标识")
            .require(true)
            .max_length(GROUP_KEY_MAX_LENGTH)
            .pattern(GROUP_KEY_PATTERN),
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
pub(super) struct CreateGroupResult {
    id: i64,
    group_key: String,
    title: String,
    description: Option<String>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CreateGroupInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        // 组本身不是权限：建组既不需要过目录校验，也不改变任何人的有效权限，
        // 因此这里没有扇出失效。`group_key` 撞唯一键时由 `From<DbError>` 折算成
        // 既有的参数错误语义，原样上抛。
        let group_id = access
            .groups()
            .insert_group_in_tx(
                &ctx,
                &mut transaction,
                &input.group_key,
                &input.title,
                input.description.as_deref(),
                operator_id,
            )
            .await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("permission_group", group_id)?,
            None,
            Some(audit::summary([("group_key", json!(input.group_key))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(group_id)
    }
    .await;
    let group_id = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        CreateGroupResult {
            id: group_id,
            group_key: input.group_key,
            title: input.title,
            description: input.description,
        },
        "权限组已创建",
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("create_group"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/groups")
        .display_name("创建权限组")
        .description("创建一个权限组")
        .permissions(["access.groups.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_pins_the_group_key_and_display_contract() {
        let injected = serde_json::from_value::<CreateGroupInput>(serde_json::json!({
            "group_key": "ops",
            "title": "运维",
            "created_by": 7
        }));
        assert!(injected.is_err(), "客户端不能注入 created_by 等内部字段");

        let params = <CreateGroupInput as ParamInput>::params();
        let param = |name: &str| {
            params
                .as_slice()
                .iter()
                .find(|param| param.name.as_str() == name)
                .unwrap_or_else(|| panic!("应声明 {name} 参数"))
        };
        let group_key = param("group_key");
        assert!(group_key.required);
        assert_eq!(
            group_key.validation.pattern.as_deref(),
            Some(GROUP_KEY_PATTERN)
        );
        assert_eq!(group_key.validation.max_length, Some(GROUP_KEY_MAX_LENGTH));
        // 展示字段的上限必须与表声明同宽：比列窄只是提前拒绝，比列宽会被数据库
        // 以「数据过长」打回成 500。
        assert_eq!(param("title").validation.max_length, Some(128));
        assert_eq!(param("description").validation.max_length, Some(255));
        assert!(
            !param("description").required,
            "描述是可选字段，缺省必须表示成 None 而不是空串"
        );
    }
}
