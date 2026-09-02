//! 授予目标用户一个已声明的权限（幂等）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::permission_catalog::{PERMISSION_MAX_LENGTH, PERMISSION_PATTERN};
use crate::addon::account;
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
    pub(super) GrantPermissionInput {
        user_id: Key::new()
            .title("目标用户")
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
struct GrantPermissionResult {
    user_id: i64,
    permission: String,
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: GrantPermissionInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    if input.user_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "user_id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    // 只能授予 Catalog 中已声明的权限（fail-closed，决策 D3）。
    access
        .permission_catalog()
        .ensure_declared(&input.permission)?;

    // 同一事务：写授权事实 + 递增目标用户授权版本 + 追加 Outbox（writer 契约）。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = account::lock_authorization_version(
            ctx.tools().mysql()?.pool(),
            &mut transaction,
            input.user_id,
        )
        .await?;
        if !locked.is_active() {
            return Err(BaseError::Unauthorized("目标用户已停用".to_string()));
        }
        if access
            .grants()
            .exists_in_tx(&ctx, &mut transaction, input.user_id, &input.permission)
            .await?
        {
            return Ok(false);
        }
        access
            .grants()
            .insert_in_tx(
                &ctx,
                &mut transaction,
                input.user_id,
                &input.permission,
                operator_id,
            )
            .await?;
        account::increment_locked_authorization_version(&mut transaction, &locked).await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("user", input.user_id)?,
            None,
            Some(audit::summary([("permission", json!(input.permission))])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(true)
    }
    .await;
    let changed = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        GrantPermissionResult {
            user_id: input.user_id,
            permission: input.permission,
            changed,
        },
        if changed {
            "权限已授予，目标用户刷新会话后生效"
        } else {
            "目标用户已持有该权限"
        },
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("grant_permission"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/grants")
        .display_name("授予权限")
        .description("授予目标用户一个已声明的权限（幂等）")
        .permissions(["access.grants.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_rejects_unknown_fields_and_blank_permission() {
        let injected = serde_json::from_value::<GrantPermissionInput>(serde_json::json!({
            "user_id": 7,
            "permission": "access.grants.read",
            "granted_by": 1
        }));
        assert!(injected.is_err(), "客户端不能注入 granted_by 等内部字段");

        let missing = serde_json::from_value::<GrantPermissionInput>(serde_json::json!({
            "user_id": 7
        }));
        assert!(missing.is_err());
    }

    #[test]
    fn params_declare_permission_format_contract() {
        let params = GrantPermissionInput::params();
        let permission = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "permission")
            .unwrap_or_else(|| panic!("应声明 permission 参数"));
        assert_eq!(
            permission.validation.pattern.as_deref(),
            Some(PERMISSION_PATTERN)
        );
        let user_id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "user_id")
            .unwrap_or_else(|| panic!("应声明 user_id 参数"));
        assert!(user_id.required);
    }
}
