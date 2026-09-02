//! `access.grants` Module（module 层）：授权管理模块装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件按分区顺序装配；
//! 业务用例全部在 `actions/` 的自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::context::Access;
use super::domain::permission_catalog::PermissionCatalogHandle;
use super::domain::repository::GrantRepository;
use crate::addon::account::user_from_claims;
use crate::authorization::AuthorizationVersionValidator;
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 装配 `access.grants` Module：表 → 上下文 → 中间件 → Action 注册表。
///
/// 共享上下文随模块一并返回：组合根用它装配账号域的 `GrantResolver` 端口。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    permission_catalog: PermissionCatalogHandle,
) -> Result<(ModuleSpec, Arc<Access>), BaseError> {
    let table = table::grants_table_spec()?;
    let access = Arc::new(Access::new(
        GrantRepository::new(table.table_definition()?),
        permission_catalog,
    ));

    let module = ModuleSpec::new(
        ModuleName::new("access.grants")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(table)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    let module = actions::register_all(module, Arc::clone(&access));
    Ok((module, access))
}
