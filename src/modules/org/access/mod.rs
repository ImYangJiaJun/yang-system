//! `org.tenant` pre-tenant 入口：在注入租户上下文之前发现与创建企业。

mod actions;
mod repository;
mod service;

use crate::modules::account;
use repository::TenantRepository;
use service::TenantService;
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::ModuleSpec;
use yang_base::table::TableDefinition;
use yang_base::BaseError;

pub(super) const ORGANIZATION_TABLE: &str = "org_org";
pub(super) const MEMBERSHIP_TABLE: &str = "org_user";

pub(super) fn build_module(
    organizations: TableDefinition,
    memberships: TableDefinition,
) -> Result<ModuleSpec, BaseError> {
    if organizations.name() != ORGANIZATION_TABLE || memberships.name() != MEMBERSHIP_TABLE {
        return Err(BaseError::ConfigError(
            "org.tenant Repository 绑定了错误的表定义".to_string(),
        ));
    }
    let service = Arc::new(TenantService::new(TenantRepository::new()));
    let module = ModuleSpec::new(yang_base::module!("org.tenant"))
        .middleware(TokenAuthMiddleware::new(account::user_from_claims));
    actions::register_all(module, service)
}
