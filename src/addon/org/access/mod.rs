//! `org.tenant` pre-tenant 入口：在注入租户上下文之前发现与创建企业。

mod actions;
mod domain;

use crate::addon::account;
use crate::authorization::AuthorizationVersionValidator;
use domain::repository::TenantRepository;
use domain::service::TenantService;
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModulePresentationSpec, ModuleSpec,
};
use yang_base::table::TableDefinition;
use yang_base::BaseError;

pub(super) const ORGANIZATION_TABLE: &str = "org_org";
pub(super) const MEMBERSHIP_TABLE: &str = "org_user";

pub(super) fn build_module(
    organizations: TableDefinition,
    memberships: TableDefinition,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    if organizations.name() != ORGANIZATION_TABLE || memberships.name() != MEMBERSHIP_TABLE {
        return Err(BaseError::ConfigError(
            "org.tenant Repository 绑定了错误的表定义".to_string(),
        ));
    }
    let service = Arc::new(TenantService::new(TenantRepository::new(
        organizations,
        memberships,
    )));
    let module = ModuleSpec::new(yang_base::module!("org.tenant")).middleware(
        TokenAuthMiddleware::new(account::user_from_claims)
            .with_claims_validator(authorization_validator),
    );
    actions::register_all(module, service).map(|module| {
        module.presentation(
            ModulePresentationSpec::new(
                crate::addon::organization_identity(),
                "我的企业",
                "organizations",
            )
            .description("发现已有企业或创建新的企业")
            .order(10)
            .primary_action(yang_base::action!("org.tenant.list"))
            .present_action(
                yang_base::action!("org.tenant.create"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            ),
        )
    })
}
