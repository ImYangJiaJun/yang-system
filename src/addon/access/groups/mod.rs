//! `access.groups` Module（module 层）：权限组管理装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表、
//! Step-up 守卫与展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::context::Access;
use super::domain::permission_catalog::PermissionCatalogHandle;
use crate::addon::account::user_from_claims;
use crate::authorization::{AuthorizationPort, AuthorizationVersionValidator, StepUpServices};
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 装配 `access.groups` Module：本任务只装表与中间件，Action 在后续任务加入。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    _step_up: Option<StepUpServices>,
    _permission_catalog: PermissionCatalogHandle,
    _authorization: AuthorizationPort,
    access: Arc<Access>,
) -> Result<ModuleSpec, BaseError> {
    let table = table::groups_table_spec()?;
    let module = ModuleSpec::new(
        ModuleName::new("access.groups").map_err(|e| BaseError::ConfigError(e.to_string()))?,
    )
    .table(table)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    // 组 CRUD Action 在 Task 10 加入注册表；此处已按同一装配路径接入，避免空注册表成为死代码。
    Ok(actions::register_all(module, access))
}
