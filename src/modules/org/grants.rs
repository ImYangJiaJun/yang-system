//! 企业管理员对 Token 授权快照的扩展。

use super::organization::{ACTIVE_STATUS as ACTIVE_ORG_STATUS, STATUS as ORG_STATUS};
use super::user::{
    ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS, IS_ADMIN, STATUS as MEMBERSHIP_STATUS, USER_ID,
};
use crate::modules::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use yang_base::action::ActionContext;
use yang_base::BaseError;

#[derive(Debug, Default)]
pub(super) struct OrgGrantResolver;

#[async_trait]
impl GrantResolver for OrgGrantResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<AuthorizationGrants, BaseError> {
        let sql = format!(
            "SELECT EXISTS(\
                 SELECT 1 FROM `org_user` AS membership \
                 INNER JOIN `org_org` AS organization \
                     ON organization.`id` = membership.`org_org` \
                 WHERE membership.`{USER_ID}` = ? \
                   AND membership.`{MEMBERSHIP_STATUS}` = ? \
                   AND membership.`{IS_ADMIN}` = TRUE \
                   AND organization.`{ORG_STATUS}` = ? \
                 LIMIT 1\
             )"
        );
        // tenant-boundary: raw-sql authorization-grant-snapshot
        let is_admin = sqlx::query_scalar::<_, bool>(&sql)
            .bind(user_id)
            .bind(ACTIVE_MEMBERSHIP_STATUS)
            .bind(ACTIVE_ORG_STATUS)
            // tenant-boundary: database authorization-grant-database
            .fetch_one(ctx.tools().mysql()?.pool())
            .await
            .map_err(yang_db::DbError::from)?;

        Ok(if is_admin {
            org_admin_grants()
        } else {
            AuthorizationGrants::default()
        })
    }
}

fn org_admin_grants() -> AuthorizationGrants {
    AuthorizationGrants::default()
        .role("org_admin")
        .permission("org.user:write")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn organization_admin_receives_only_member_write_permission() {
        let grants = org_admin_grants();
        assert_eq!(grants.roles().collect::<Vec<_>>(), ["org_admin"]);
        assert_eq!(grants.permissions().collect::<Vec<_>>(), ["org.user:write"]);
    }
}
