//! 查询目标用户的全部直授权限。

use crate::addon::access::domain::context::Access;
use crate::addon::account;
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ListUserGrantsInput {
        #[param(source = path)]
        user_id: Key::new()
            .title("目标用户")
            .require(true),
    }
}

/// 单条直授权限的对外视图。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct GrantView {
    id: i64,
    permission: String,
    granted_by: i64,
    occurred_at: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListUserGrantsResult {
    user_id: i64,
    grants: Vec<GrantView>,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ListUserGrantsInput,
    access: Arc<Access>,
) -> Result<ListUserGrantsResult, BaseError> {
    if input.user_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "user_id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    // 目标用户必须存在；停用用户的授权事实仍允许查询（审计需要）。
    if account::find_authorization_version(ctx.tools().mysql()?.pool(), input.user_id)
        .await?
        .is_none()
    {
        return Err(BaseError::UserNotFound(input.user_id.to_string()));
    }

    let mut transaction = ctx
        .tools()
        .mysql()?
        .read_only_transaction()
        .await
        .map_err(BaseError::from)?;
    let result = access
        .grants()
        .list_by_user_in_tx(&ctx, &mut transaction, input.user_id)
        .await;
    let records = match result {
        Ok(records) => {
            transaction.commit().await.map_err(BaseError::from)?;
            records
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!(error = %rollback_error, "查询用户授权事务回滚失败");
            }
            return Err(error);
        }
    };

    Ok(ListUserGrantsResult {
        user_id: input.user_id,
        grants: records
            .into_iter()
            .map(|record| GrantView {
                id: record.id,
                permission: record.permission,
                granted_by: record.granted_by,
                occurred_at: record.occurred_at,
            })
            .collect(),
    })
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_user_grants"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Get, "/api/v1/access/users/{user_id}/grants")
        .display_name("用户授权列表")
        .description("查询目标用户的全部直授权限")
        .permissions(["access.grants.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::{ParamInput, ParamSource};

    #[test]
    fn user_id_comes_from_the_path_not_the_body() {
        let params = ListUserGrantsInput::params();
        let user_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "user_id")
            .unwrap_or_else(|| panic!("应声明 user_id 参数"));
        assert_eq!(user_id.source, ParamSource::Path);
        assert!(user_id.required);

        let injected = serde_json::from_value::<ListUserGrantsInput>(serde_json::json!({
            "user_id": 7,
            "permission": "access.grants.write"
        }));
        assert!(injected.is_err(), "客户端不能注入过滤字段");
    }
}
