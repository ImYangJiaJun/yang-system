//! 企业管理员对 Token 授权快照的扩展。
//! raw-sql-boundary: domain-service org-grant-snapshot

use super::organization::ACTIVE_STATUS as ACTIVE_ORG_STATUS;
use super::user::ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS;
use crate::modules::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

#[derive(Debug, Default)]
pub(super) struct OrgGrantResolver;

#[async_trait]
impl GrantResolver for OrgGrantResolver {
    async fn resolve(
        &self,
        _ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        let executor = transaction.executor().ok_or_else(|| {
            BaseError::from(yang_db::DbError::TransactionError(
                "授权快照事务已结束".to_string(),
            ))
        })?;
        // tenant-boundary: raw-sql authorization-grant-snapshot
        let is_admin = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                 SELECT 1 FROM `org_user` AS membership \
                 INNER JOIN `org_org` AS organization \
                     ON organization.`id` = membership.`org_org` \
                 WHERE membership.`user_user` = ? \
                   AND membership.`status` = ? \
                   AND membership.`admin` = TRUE \
                   AND organization.`status` = ? \
                 LIMIT 1\
             )",
        )
        .bind(user_id)
        .bind(ACTIVE_MEMBERSHIP_STATUS)
        .bind(ACTIVE_ORG_STATUS)
        .fetch_one(executor)
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
