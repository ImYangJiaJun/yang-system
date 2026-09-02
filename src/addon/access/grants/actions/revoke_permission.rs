//! 撤销目标用户的一个直授权限（幂等）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::permission_catalog::{PERMISSION_MAX_LENGTH, PERMISSION_PATTERN};
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
    pub(super) RevokePermissionInput {
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
struct RevokePermissionResult {
    user_id: i64,
    permission: String,
    changed: bool,
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RevokePermissionInput,
    access: Arc<Access>,
) -> Result<ApiResponse, BaseError> {
    let operator_id = ctx.actor()?.user_id();
    if input.user_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "user_id".to_string(),
            "目标用户必须是正整数".to_string(),
        ));
    }
    // 撤销不做目录成员校验：已从 Catalog 移除的权限也必须能清理。

    // 同一事务：删授权事实 + 递增目标用户授权版本 + 追加 Outbox（writer 契约）。
    let mut transaction = ctx.tools().mysql()?.transaction().await?;
    let result = async {
        let locked = access
            .authorization()
            .lock_authorization_version(
                ctx.tools().mysql()?.pool(),
                &mut transaction,
                input.user_id,
            )
            .await?;
        let removed = access
            .grants()
            .delete_in_tx(&ctx, &mut transaction, input.user_id, &input.permission)
            .await?;
        if removed == 0 {
            return Ok(false);
        }
        access
            .authorization()
            .increment_locked_authorization_version(&mut transaction, &locked)
            .await?;
        let event = audit::succeeded_event(
            &ctx,
            None,
            Some(audit::entity("user", operator_id)?),
            audit::entity("user", input.user_id)?,
            Some(audit::summary([("permission", json!(input.permission))])?),
            None,
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(true)
    }
    .await;
    let changed = Access::finish_transaction(transaction, result).await?;

    ApiResponse::success(
        RevokePermissionResult {
            user_id: input.user_id,
            permission: input.permission,
            changed,
        },
        if changed {
            "权限已撤销，目标用户刷新会话后生效"
        } else {
            "目标用户本就没有该权限"
        },
    )
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("revoke_permission"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Post, "/api/v1/access/grants/revoke")
        .display_name("撤销权限")
        .description("撤销目标用户的一个直授权限（幂等）")
        .permissions(["access.grants.write"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_rejects_unknown_fields() {
        let injected = serde_json::from_value::<RevokePermissionInput>(serde_json::json!({
            "user_id": 7,
            "permission": "access.grants.read",
            "reason": "cleanup"
        }));
        assert!(injected.is_err(), "客户端不能注入额外字段");

        let missing = serde_json::from_value::<RevokePermissionInput>(serde_json::json!({
            "permission": "access.grants.read"
        }));
        assert!(missing.is_err());
    }
}
