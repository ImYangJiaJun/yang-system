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
use crate::authorization::{
    AuthorizationPort, AuthorizationVersionValidator, RequestFingerprintResolver, StepUpServices,
};
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 装配 `access.groups` Module：表 → 中间件 → Action 注册表 → Step-up 守卫。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
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
    if let Some(step_up) = step_up {
        for target in step_up_targets() {
            module = module.middleware(
                step_up.middleware(target, RequestFingerprintResolver::global("access-groups")),
            );
        }
    }
    Ok(module)
}

/// 需要 Step-up 重认证的组写操作：建/改/删都直接改变授权事实或其载体。
fn step_up_targets() -> Vec<yang_base::definition::ActionRef> {
    vec![
        yang_base::action!("access.groups.create_group"),
        yang_base::action!("access.groups.update_group"),
        yang_base::action!("access.groups.delete_group"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_mutation_is_explicitly_step_up_protected() {
        assert_eq!(
            step_up_targets(),
            vec![
                yang_base::action!("access.groups.create_group"),
                yang_base::action!("access.groups.update_group"),
                yang_base::action!("access.groups.delete_group"),
            ],
            "组写操作必须逐个登记，漏掉一个就是无重认证的授权变更入口"
        );
    }
}
