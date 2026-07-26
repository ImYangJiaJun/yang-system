//! 企业成员写操作的实时租户管理员校验。

use super::{ACTIVE_STATUS, IS_ADMIN, ORG_ID, STATUS, USER_ID};
use async_trait::async_trait;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::router::{Middleware, Next};
use yang_base::BaseError;

#[derive(Debug, Clone, Copy, Default)]
pub(in crate::modules::org) struct OrgAdminGuardMiddleware;

impl OrgAdminGuardMiddleware {
    pub(in crate::modules::org) fn new() -> Self {
        Self
    }

    async fn is_active_admin(&self, ctx: &ActionContext) -> Result<bool, BaseError> {
        let user = ctx
            .authenticated_user()
            .ok_or_else(|| BaseError::Unauthorized("企业成员管理需要已认证用户".to_string()))?;
        if user.has_role("system") {
            return Ok(true);
        }
        let org_id = ctx
            .tenant()?
            .id()
            .ok_or_else(|| BaseError::PermissionDenied("企业成员管理需要明确租户".to_string()))?;
        let sql = format!(
            "SELECT EXISTS(\
                 SELECT 1 FROM `org_user` \
                 WHERE `{ORG_ID}` = ? \
                   AND `{USER_ID}` = ? \
                   AND `{STATUS}` = ? \
                   AND `{IS_ADMIN}` = TRUE \
                 LIMIT 1\
             )"
        );
        // tenant-boundary: raw-sql member-admin-guard
        sqlx::query_scalar::<_, bool>(&sql)
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

#[async_trait]
impl Middleware for OrgAdminGuardMiddleware {
    async fn handle(&self, ctx: ActionContext, next: Next<'_>) -> Result<ApiResponse, BaseError> {
        let Some((module, action)) = ctx.dispatch_target() else {
            return Err(BaseError::ConfigError(
                "企业管理员校验缺少可信派发目标".to_string(),
            ));
        };
        if !requires_org_admin(module, action) {
            return next.run(ctx).await;
        }
        if !self.is_active_admin(&ctx).await? {
            return Err(BaseError::PermissionDenied(
                "当前用户不是该企业的有效管理员".to_string(),
            ));
        }
        next.run(ctx).await
    }
}

fn requires_org_admin(module: &str, action: &str) -> bool {
    module == "org.user" && matches!(action, "add" | "put" | "del")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_covers_only_member_mutations() {
        for action in ["add", "put", "del"] {
            assert!(requires_org_admin("org.user", action));
        }
        for action in ["get", "select", "table"] {
            assert!(!requires_org_admin("org.user", action));
        }
        assert!(!requires_org_admin("org.org", "put"));
    }
}
