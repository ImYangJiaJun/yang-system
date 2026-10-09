//! `access.groups` Module（module 层）：权限组管理装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表与
//! 展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::context::Access;
use super::domain::permission_catalog::PermissionCatalogHandle;
use crate::addon::account::user_from_claims;
use crate::authorization::{AuthorizationPort, AuthorizationVersionValidator};
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 装配 `access.groups` Module：表 → 中间件 → Action 注册表。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    _permission_catalog: PermissionCatalogHandle,
    _authorization: AuthorizationPort,
    access: Arc<Access>,
) -> Result<ModuleSpec, BaseError> {
    let table = table::groups_table_spec()?;
    let mut module = ModuleSpec::new(
        ModuleName::new("access.groups").map_err(|e| BaseError::ConfigError(e.to_string()))?,
    )
    .table(table)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    module = actions::register_all(module, Arc::clone(&access));
    Ok(module)
}
