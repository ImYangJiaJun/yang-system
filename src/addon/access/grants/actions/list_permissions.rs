//! 查询权限目录：全部 Action/Module 声明的权限集合（决策 D3 投影）。

use crate::addon::access::domain::context::Access;
use crate::addon::access::domain::permission_catalog::PermissionEntry;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyInput {}

impl ParamInput for EmptyInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ListPermissionsResult {
    permissions: Vec<PermissionEntry>,
}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: EmptyInput,
    access: Arc<Access>,
) -> Result<ListPermissionsResult, BaseError> {
    Ok(ListPermissionsResult {
        permissions: access.permission_catalog().entries()?.to_vec(),
    })
}

/// 自包含注册：路由/权限声明与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("list_permissions"),
            move |ctx, input| handle(ctx, input, Arc::clone(&access)),
        )
        .route(HttpMethod::Get, "/api/v1/access/permissions")
        .display_name("权限目录")
        .description("查询全部 Action 声明的权限集合")
        .permissions(["access.grants.read"])
        .register()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_rejects_any_client_supplied_field() {
        let injected = serde_json::from_value::<EmptyInput>(serde_json::json!({
            "permission": "access.grants.write"
        }));
        assert!(injected.is_err(), "目录查询不接受任何过滤字段");
    }
}
