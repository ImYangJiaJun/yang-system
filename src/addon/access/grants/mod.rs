//! `access.grants` Module（module 层）：授权管理模块装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表、
//! Step-up 守卫与展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::context::Access;
use super::domain::permission_catalog::PermissionCatalogHandle;
use super::domain::repository::GrantRepository;
use crate::addon::account::user_from_claims;
use crate::authorization::{
    AuthorizationVersionValidator, RequestFingerprintResolver, StepUpServices,
};
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModuleName, ModulePresentationSpec,
    ModuleSpec,
};
use yang_base::BaseError;

/// 装配 `access.grants` Module：表 → 上下文 → 中间件 → Action 注册表 → Step-up → 展示投影。
///
/// 共享上下文随模块一并返回：组合根用它装配账号域的 `GrantResolver` 端口。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
    permission_catalog: PermissionCatalogHandle,
) -> Result<(ModuleSpec, Arc<Access>), BaseError> {
    let table = table::grants_table_spec()?;
    let access = Arc::new(Access::new(
        GrantRepository::new(table.table_definition()?),
        permission_catalog,
    ));

    let mut module = ModuleSpec::new(
        ModuleName::new("access.grants")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
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
                step_up.middleware(target, RequestFingerprintResolver::global("access-grants")),
            );
        }
    }
    Ok((module.presentation(presentation()), access))
}

/// 前端展示投影（权限管理导航）。
fn presentation() -> ModulePresentationSpec {
    ModulePresentationSpec::new(crate::addon::user_identity(), "权限管理", "access")
        .description("管理用户直授权限与权限目录")
        .order(20)
        .primary_action(yang_base::action!("access.grants.list_user_grants"))
        .present_action(
            yang_base::action!("access.grants.grant_permission"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
        .present_action(
            yang_base::action!("access.grants.revoke_permission"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
        )
}

/// 需要 Step-up 重认证的授权写操作。
fn step_up_targets() -> Vec<yang_base::definition::ActionRef> {
    vec![
        yang_base::action!("access.grants.grant_permission"),
        yang_base::action!("access.grants.revoke_permission"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grant_mutation_is_explicitly_step_up_protected() {
        assert_eq!(
            step_up_targets(),
            vec![
                yang_base::action!("access.grants.grant_permission"),
                yang_base::action!("access.grants.revoke_permission"),
            ]
        );
    }
}
