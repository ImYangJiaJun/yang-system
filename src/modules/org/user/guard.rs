//! 企业成员写操作的实时租户管理员校验。
//! raw-sql-boundary: domain-service org-member-guard

use super::ACTIVE_STATUS;
use crate::authorization::{resource_authorization_checkpoint, ResourceAuthorizationCheckpoint};
use async_trait::async_trait;
use yang_base::action::{ActionContext, ApiResponse, SystemTenantCapability, User};
use yang_base::definition::ActionRef;
use yang_base::router::{Middleware, Next};
use yang_base::BaseError;

#[derive(Debug, Clone)]
pub(in crate::modules::org) struct OrgAdminGuardMiddleware {
    target: ActionRef,
}

impl OrgAdminGuardMiddleware {
    pub(in crate::modules::org) fn new(target: ActionRef) -> Self {
        Self { target }
    }

    async fn is_active_admin(&self, ctx: &ActionContext) -> Result<bool, BaseError> {
        let user = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("企业成员管理需要已认证用户".to_string()))?;
        if user.has_role("system") {
            // tenant-boundary: system-capability member-admin-system
            let capability = ctx.system_tenant()?;
            if !system_capability_matches_user(capability, user) {
                return Err(BaseError::PermissionDenied(
                    "系统租户 capability 与当前操作者不匹配".to_string(),
                ));
            }
            return Ok(true);
        }
        let org_id = ctx.tenant()?.id();
        // tenant-boundary: raw-sql member-admin-guard
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                 SELECT 1 FROM `org_user` \
                 WHERE `org_org` = ? \
                   AND `user_user` = ? \
                   AND `status` = ? \
                   AND `admin` = TRUE \
                 LIMIT 1\
             )",
        )
        .bind(org_id.get())
        .bind(user.id)
        .bind(ACTIVE_STATUS)
        // tenant-boundary: database member-admin-database
        .fetch_one(ctx.tools().mysql()?.pool())
        .await
        .map_err(yang_db::DbError::from)
        .map_err(Into::into)
    }
}

fn system_capability_matches_user(capability: SystemTenantCapability, user: &User) -> bool {
    capability.actor().user_id() == user.id
}

#[async_trait]
impl Middleware for OrgAdminGuardMiddleware {
    fn target_action(&self) -> Option<&ActionRef> {
        Some(&self.target)
    }

    async fn handle(&self, ctx: ActionContext, next: Next<'_>) -> Result<ApiResponse, BaseError> {
        if !self.is_active_admin(&ctx).await? {
            return Err(BaseError::PermissionDenied(
                "当前用户不是该企业的有效管理员".to_string(),
            ));
        }
        resource_authorization_checkpoint(&ctx, ResourceAuthorizationCheckpoint::AfterPrecheck)
            .await;
        next.run(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::action::TenantResolution;

    #[test]
    fn guard_is_bound_to_one_exact_registry_action() {
        let target = yang_base::action!("org.user.put");
        let guard = OrgAdminGuardMiddleware::new(target.clone());
        assert_eq!(guard.target_action(), Some(&target));
    }

    #[test]
    fn system_capability_is_bound_to_the_authenticated_actor() {
        let system = User::new(9, "system").with_roles(["system"]);
        let capability = match TenantResolution::system_for(&system)
            .unwrap_or_else(|error| panic!("system 角色应获得 capability: {error}"))
        {
            TenantResolution::System(capability) => capability,
            TenantResolution::Tenant(_) => panic!("system 角色不得获得普通租户"),
        };
        assert!(system_capability_matches_user(capability, &system));
        assert!(!system_capability_matches_user(
            capability,
            &User::new(10, "other-system").with_roles(["system"])
        ));
    }
}
