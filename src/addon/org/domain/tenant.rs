//! 企业租户的可信解析策略。

use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{
    ActionContext, ContextKey, TenantContext, TenantId, TenantResolution, TenantResolver,
};
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, QueryBuilder};

pub(in crate::addon::org) const ORG_MEMBERSHIP_CAPABILITY: ContextKey<OrgMembershipCapability> =
    ContextKey::new("org_membership_capability");

/// 同一次租户事实查询签发的请求级成员 capability。
///
/// 它只消除事务前的重复查询；写事务仍必须锁定并复核当前管理员事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::addon::org) struct OrgMembershipCapability {
    actor_id: i64,
    tenant_id: TenantId,
    is_admin: bool,
}

impl OrgMembershipCapability {
    fn new(actor_id: i64, tenant_id: TenantId, is_admin: bool) -> Self {
        Self {
            actor_id,
            tenant_id,
            is_admin,
        }
    }

    pub(in crate::addon::org) fn authorizes_admin_precheck(
        self,
        actor_id: i64,
        tenant_id: TenantId,
    ) -> bool {
        self.actor_id == actor_id && self.tenant_id == tenant_id && self.is_admin
    }
}

#[async_trait]
trait OrgMembershipReader: Send + Sync + 'static {
    async fn resolve(
        &self,
        context: &ActionContext,
        user_id: i64,
        org_id: TenantId,
    ) -> Result<Option<OrgMembershipCapability>, BaseError>;
}

/// 使用单次 JOIN 同时核验成员、企业状态并投影管理员能力的生产实现。
struct DatabaseMembershipReader;

#[async_trait]
impl OrgMembershipReader for DatabaseMembershipReader {
    async fn resolve(
        &self,
        context: &ActionContext,
        user_id: i64,
        org_id: TenantId,
    ) -> Result<Option<OrgMembershipCapability>, BaseError> {
        // tenant-boundary: database tenant-membership-capability-database
        let pool = context.tools().mysql()?.pool();
        let is_admin: Option<bool> = QueryBuilder::from_pool(pool, table!("org_user"))
            .field(field!("org_user.admin"))
            .join(
                table!("org_org"),
                field!("org_org.id"),
                field!("org_user.org_org"),
            )
            .where_and(field!("org_user.org_org"), CompareOp::Eq, org_id.get())
            .where_and(field!("org_user.user_user"), CompareOp::Eq, user_id)
            .where_and(field!("org_user.status"), CompareOp::Eq, "active")
            .where_and(field!("org_org.status"), CompareOp::Eq, "active")
            .find::<(bool,)>()
            .await?
            .map(|(admin,)| admin);
        Ok(is_admin.map(|admin| OrgMembershipCapability::new(user_id, org_id, admin)))
    }
}

/// 从已认证用户和请求声明中解析可信租户，默认采用 fail-closed 策略。
#[derive(Clone)]
pub(in crate::addon::org) struct OrgTenantResolver {
    memberships: Arc<dyn OrgMembershipReader>,
}

impl OrgTenantResolver {
    fn new(memberships: Arc<dyn OrgMembershipReader>) -> Self {
        Self { memberships }
    }

    /// 构造生产 resolver，不向 Addon 根泄漏内部读取器抽象。
    pub(in crate::addon::org) fn database() -> Self {
        Self::new(Arc::new(DatabaseMembershipReader))
    }

    async fn resolve_authenticated_with_capability(
        &self,
        context: &ActionContext,
        user: &yang_base::action::User,
        requested: Option<TenantId>,
    ) -> Result<(TenantResolution, Option<OrgMembershipCapability>), BaseError> {
        if user.has_role("system") {
            return Ok((TenantResolution::system_for(user)?, None));
        }
        let org_id = requested
            .ok_or_else(|| BaseError::Unauthorized("请求缺少企业租户上下文".to_string()))?;
        let Some(capability) = self.memberships.resolve(context, user.id, org_id).await? else {
            return Err(BaseError::PermissionDenied(format!(
                "用户无权访问企业 {}",
                org_id.get()
            )));
        };
        Ok((TenantContext::new(org_id).into(), Some(capability)))
    }

    async fn resolve_authenticated(
        &self,
        context: &ActionContext,
        user: &yang_base::action::User,
        requested: Option<TenantId>,
    ) -> Result<TenantResolution, BaseError> {
        self.resolve_authenticated_with_capability(context, user, requested)
            .await
            .map(|(resolution, _)| resolution)
    }
}

#[async_trait]
impl TenantResolver for OrgTenantResolver {
    async fn resolve(
        &self,
        context: &ActionContext,
        requested: Option<TenantId>,
    ) -> Result<TenantResolution, BaseError> {
        let user = context
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("企业租户解析需要已认证用户".to_string()))?;
        self.resolve_authenticated(context, user, requested).await
    }

    async fn resolve_with_context(
        &self,
        context: &mut ActionContext,
        requested: Option<TenantId>,
    ) -> Result<TenantResolution, BaseError> {
        let user = context
            .authenticated_user()
            .cloned()
            .ok_or_else(|| BaseError::Unauthorized("企业租户解析需要已认证用户".to_string()))?;
        let (resolution, capability) = self
            .resolve_authenticated_with_capability(context, &user, requested)
            .await?;
        if let Some(capability) = capability {
            context
                .request_context()
                .insert(ORG_MEMBERSHIP_CAPABILITY, capability);
        }
        Ok(resolution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::action::{Request, User};
    use yang_base::tools::ToolsBuilder;

    struct FakeMembershipReader;

    #[async_trait]
    impl OrgMembershipReader for FakeMembershipReader {
        async fn resolve(
            &self,
            _context: &ActionContext,
            user_id: i64,
            org_id: TenantId,
        ) -> Result<Option<OrgMembershipCapability>, BaseError> {
            Ok((user_id == 7 && org_id == TenantId::new(10))
                .then(|| OrgMembershipCapability::new(user_id, org_id, true)))
        }
    }

    fn context() -> ActionContext {
        ActionContext::new(
            Request::new(serde_json::json!({})),
            Arc::new(
                ToolsBuilder::new()
                    .build()
                    .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
            ),
        )
    }

    #[tokio::test]
    async fn tenant_policy_rejects_missing_and_cross_tenant_but_accepts_membership() {
        let resolver = OrgTenantResolver::new(Arc::new(FakeMembershipReader));
        let member = User::new(7, "member");

        assert!(matches!(
            resolver
                .resolve_authenticated(&context(), &member, None)
                .await,
            Err(BaseError::Unauthorized(_))
        ));
        assert!(matches!(
            resolver
                .resolve_authenticated(&context(), &member, Some(TenantId::new(20)))
                .await,
            Err(BaseError::PermissionDenied(_))
        ));
        let resolution = resolver
            .resolve_authenticated(&context(), &member, Some(TenantId::new(10)))
            .await
            .unwrap_or_else(|error| panic!("真实成员应通过租户策略: {error}"));
        match resolution {
            TenantResolution::Tenant(tenant) => {
                assert_eq!(tenant.id(), TenantId::new(10));
            }
            TenantResolution::System(_) => panic!("普通成员不得获得系统 capability"),
        }
    }

    #[tokio::test]
    async fn system_role_receives_actor_bound_capability() {
        let resolver = OrgTenantResolver::new(Arc::new(FakeMembershipReader));
        let system = User::new(9, "system").with_roles(["system"]);

        let resolution = resolver
            .resolve_authenticated(&context(), &system, None)
            .await
            .unwrap_or_else(|error| panic!("system 角色应显式绕过普通租户选择: {error}"));
        match resolution {
            TenantResolution::System(capability) => {
                assert_eq!(capability.actor().user_id(), system.id);
            }
            TenantResolution::Tenant(_) => panic!("system 角色不得伪装成普通租户"),
        }
    }

    #[test]
    fn membership_capability_is_bound_to_actor_tenant_and_admin_fact() {
        let admin = OrgMembershipCapability::new(7, TenantId::new(10), true);
        assert!(admin.authorizes_admin_precheck(7, TenantId::new(10)));
        assert!(!admin.authorizes_admin_precheck(8, TenantId::new(10)));
        assert!(!admin.authorizes_admin_precheck(7, TenantId::new(20)));
        assert!(!OrgMembershipCapability::new(7, TenantId::new(10), false)
            .authorizes_admin_precheck(7, TenantId::new(10)));
    }
}
