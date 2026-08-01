//! 个人任务规划 Addon 的公开组装入口。

mod grants;
mod project;
mod task;
mod tenant;

use crate::addon::account;
use crate::authorization::AuthorizationVersionValidator;
use yang_base::action::{TenantResolverMiddleware, TokenAuthMiddleware};
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

pub(crate) use grants::grant_resolver;

/// 构建个人任务规划 Addon。
///
/// 个人工作区使用已认证用户 ID 作为不可伪造的租户 capability；所有主表均声明
/// `owner_user` tenant key，因此通用查询与 CRUD 自动获得相同的失败关闭边界。
pub fn build_addon(
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    let project = project::build_module()?
        .middleware(
            TokenAuthMiddleware::new(account::user_from_claims)
                .with_claims_validator(authorization_validator.clone()),
        )
        .middleware(TenantResolverMiddleware::new(
            tenant::PersonalWorkspaceResolver,
        ));
    let task = task::build_module()?
        .middleware(
            TokenAuthMiddleware::new(account::user_from_claims)
                .with_claims_validator(authorization_validator),
        )
        .middleware(TenantResolverMiddleware::new(
            tenant::PersonalWorkspaceResolver,
        ));

    Ok(AddonSpec::new(yang_base::addon!("work"))
        .depends_on(yang_base::addon!("account"))
        .module(project)
        .module(task))
}
