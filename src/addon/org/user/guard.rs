//! 企业成员写操作的请求 capability 快速校验。

use crate::addon::org::tenant::ORG_MEMBERSHIP_CAPABILITY;
use crate::authorization::{resource_authorization_checkpoint, ResourceAuthorizationCheckpoint};
use async_trait::async_trait;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::ActionRef;
use yang_base::router::{Middleware, Next};
use yang_base::BaseError;

#[derive(Debug, Clone)]
pub(in crate::addon::org) struct OrgAdminGuardMiddleware {
    target: ActionRef,
}

impl OrgAdminGuardMiddleware {
    pub(in crate::addon::org) fn new(target: ActionRef) -> Self {
        Self { target }
    }

    fn is_active_admin(&self, ctx: &mut ActionContext) -> Result<bool, BaseError> {
        let (user_id, is_system) = ctx
            .authenticated_user()
            .map(|user| (user.id, user.has_role("system")))
            .ok_or_else(|| BaseError::Unauthorized("企业成员管理需要已认证用户".to_string()))?;
        if is_system {
            // tenant-boundary: system-capability member-admin-system
            let capability = ctx.system_tenant()?;
            if capability.actor().user_id() != user_id {
                return Err(BaseError::PermissionDenied(
                    "系统租户 capability 与当前操作者不匹配".to_string(),
                ));
            }
            return Ok(true);
        }
        let org_id = ctx.tenant()?.id();
        let capability = *ctx.request_context().require(ORG_MEMBERSHIP_CAPABILITY)?;
        Ok(capability.authorizes_admin_precheck(user_id, org_id))
    }
}

#[cfg(test)]
fn system_capability_matches_user(
    capability: yang_base::action::SystemTenantCapability,
    user: &yang_base::action::User,
) -> bool {
    capability.actor().user_id() == user.id
}

#[async_trait]
impl Middleware for OrgAdminGuardMiddleware {
    fn target_action(&self) -> Option<&ActionRef> {
        Some(&self.target)
    }

    async fn handle(
        &self,
        mut ctx: ActionContext,
        next: Next<'_>,
    ) -> Result<ApiResponse, BaseError> {
        if !self.is_active_admin(&mut ctx)? {
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
    use yang_base::action::{TenantResolution, User};

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
