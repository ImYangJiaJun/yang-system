//! 企业租户的可信解析策略。

use super::organization::{ACTIVE_STATUS as ACTIVE_ORG_STATUS, STATUS as ORG_STATUS};
use super::user::{
    ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS, ORG_ID, STATUS as MEMBERSHIP_STATUS, USER_ID,
};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::{ActionContext, TenantContext, TenantId, TenantResolver};
use yang_base::table::TableDefinition;
use yang_base::BaseError;

#[async_trait]
trait OrgMembershipReader: Send + Sync + 'static {
    async fn contains(
        &self,
        context: &ActionContext,
        user_id: i64,
        org_id: TenantId,
    ) -> Result<bool, BaseError>;
}

/// 使用 `org_user` 表核验成员关系的生产实现。
struct DatabaseMembershipReader {
    memberships: TableDefinition,
    organizations: TableDefinition,
}

impl DatabaseMembershipReader {
    fn new(memberships: TableDefinition, organizations: TableDefinition) -> Self {
        Self {
            memberships,
            organizations,
        }
    }
}

#[async_trait]
impl OrgMembershipReader for DatabaseMembershipReader {
    async fn contains(
        &self,
        context: &ActionContext,
        user_id: i64,
        org_id: TenantId,
    ) -> Result<bool, BaseError> {
        let table = self
            .memberships
            .bind(Arc::new(context.tools().mysql()?.pool().clone()));
        let rows = table
            .query(std::iter::empty::<&str>())
            .select_fields(&["id"])?
            .where_eq(ORG_ID, serde_json::json!(org_id.get()))?
            .where_eq(USER_ID, serde_json::json!(user_id))?
            .where_eq(
                MEMBERSHIP_STATUS,
                serde_json::json!(ACTIVE_MEMBERSHIP_STATUS),
            )?
            .page(1, 1)?
            .all()
            .await?;
        if rows.is_empty() {
            return Ok(false);
        }

        let organizations = self
            .organizations
            .bind(Arc::new(context.tools().mysql()?.pool().clone()));
        let rows = organizations
            .query(std::iter::empty::<&str>())
            .select_fields(&["id"])?
            .where_eq("id", serde_json::json!(org_id.get()))?
            .where_eq(ORG_STATUS, serde_json::json!(ACTIVE_ORG_STATUS))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }
}

/// 从已认证用户和请求声明中解析可信租户，默认采用 fail-closed 策略。
#[derive(Clone)]
pub(super) struct OrgTenantResolver {
    memberships: Arc<dyn OrgMembershipReader>,
}

impl OrgTenantResolver {
    fn new(memberships: Arc<dyn OrgMembershipReader>) -> Self {
        Self { memberships }
    }

    /// 从成员关系表构造生产 resolver，不向 Addon 根泄漏内部读取器抽象。
    pub(super) fn from_tables(
        memberships: TableDefinition,
        organizations: TableDefinition,
    ) -> Self {
        Self::new(Arc::new(DatabaseMembershipReader::new(
            memberships,
            organizations,
        )))
    }

    async fn resolve_authenticated(
        &self,
        context: &ActionContext,
        user: &yang_base::action::User,
        requested: Option<TenantId>,
    ) -> Result<TenantContext, BaseError> {
        if user.has_role("system") {
            return Ok(TenantContext::system());
        }
        let org_id = requested
            .ok_or_else(|| BaseError::Unauthorized("请求缺少企业租户上下文".to_string()))?;
        if !self.memberships.contains(context, user.id, org_id).await? {
            return Err(BaseError::PermissionDenied(format!(
                "用户无权访问企业 {}",
                org_id.get()
            )));
        }
        Ok(TenantContext::new(org_id))
    }
}

#[async_trait]
impl TenantResolver for OrgTenantResolver {
    async fn resolve(
        &self,
        context: &ActionContext,
        requested: Option<TenantId>,
    ) -> Result<TenantContext, BaseError> {
        let user = context
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("企业租户解析需要已认证用户".to_string()))?;
        self.resolve_authenticated(context, user, requested).await
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
        async fn contains(
            &self,
            _context: &ActionContext,
            user_id: i64,
            org_id: TenantId,
        ) -> Result<bool, BaseError> {
            Ok(user_id == 7 && org_id == TenantId::new(10))
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
        let tenant = resolver
            .resolve_authenticated(&context(), &member, Some(TenantId::new(10)))
            .await
            .unwrap_or_else(|error| panic!("真实成员应通过租户策略: {error}"));
        assert_eq!(tenant.id(), Some(TenantId::new(10)));
        assert!(!tenant.is_system());
    }

    #[tokio::test]
    async fn system_role_is_the_only_explicit_tenant_bypass() {
        let resolver = OrgTenantResolver::new(Arc::new(FakeMembershipReader));
        let system = User::new(9, "system").with_roles(["system"]);

        let tenant = resolver
            .resolve_authenticated(&context(), &system, None)
            .await
            .unwrap_or_else(|error| panic!("system 角色应显式绕过普通租户选择: {error}"));
        assert!(tenant.is_system());
        assert_eq!(tenant.id(), None);
    }
}
