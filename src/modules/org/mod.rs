//! 企业 Addon 的公开组装入口。

mod access;
mod grants;
mod organization;
mod pagination;
mod tenant;
mod user;

use crate::authorization::AuthorizationVersionValidator;
use crate::authorization::{RequestFingerprintResolver, StepUpServices};
use crate::modules::account;
use crate::modules::account::GrantResolver;
use grants::OrgGrantResolver;
use std::sync::Arc;
use yang_base::action::{TenantResolverMiddleware, TokenAuthMiddleware};
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

/// 返回企业账号域的 Token 授权解析器。
pub(crate) fn grant_resolver() -> Arc<dyn GrantResolver> {
    Arc::new(OrgGrantResolver)
}

/// 构建企业 Addon。
///
/// 此处只负责 Module 组合、Addon 依赖和跨 Module 中间件；企业查询、成员 Schema、
/// 租户校验分别由子模块维护，避免 `mod.rs` 演变为业务实现文件。
pub fn build_addon(
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
) -> Result<AddonSpec, BaseError> {
    let organization = organization::build_module();
    let members = user::build_module()?;
    let organization_table = organization
        .table
        .as_ref()
        .ok_or(BaseError::TableDefinitionNotSet)?
        .table_definition()?;
    let membership_table = members
        .table
        .as_ref()
        .ok_or(BaseError::TableDefinitionNotSet)?
        .table_definition()?;
    let resolver = tenant::OrgTenantResolver::database();
    let access = access::build_module(
        organization_table,
        membership_table,
        authorization_validator.clone(),
    )?;

    // 中间件顺序具有语义：Token 认证先写入可信用户，租户解析随后校验企业成员关系。
    let organization = organization
        .middleware(
            TokenAuthMiddleware::new(account::user_from_claims)
                .with_claims_validator(authorization_validator.clone()),
        )
        .middleware(TenantResolverMiddleware::new(resolver.clone()));
    let members = members
        .middleware(
            TokenAuthMiddleware::new(account::user_from_claims)
                .with_claims_validator(authorization_validator),
        )
        .middleware(TenantResolverMiddleware::new(resolver));
    let members = user::register_resource_authorizers(members);
    let mut members = members;
    if let Some(step_up) = step_up {
        for target in user::step_up_targets() {
            members = members.middleware(
                step_up.middleware(target, RequestFingerprintResolver::tenant("org-user")),
            );
        }
    }

    Ok(AddonSpec::new(yang_base::addon!("org"))
        .depends_on(yang_base::addon!("account"))
        .module(access)
        .module(organization)
        .module(members))
}
