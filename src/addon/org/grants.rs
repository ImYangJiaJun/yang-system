//! 企业管理员对 Token 授权快照的扩展。

use super::organization::ACTIVE_STATUS as ACTIVE_ORG_STATUS;
use super::user::ACTIVE_STATUS as ACTIVE_MEMBERSHIP_STATUS;
use crate::addon::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, Transaction};

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
        // 原 SELECT EXISTS(... JOIN ...) 改写为同事务内的 join + count，保持事务快照可见性。
        let admin_count = transaction
            .table(table!("org_user"))
            .join(
                table!("org_org"),
                field!("org_org.id"),
                field!("org_user.org_org"),
            )
            .where_and(field!("org_user.user_user"), CompareOp::Eq, user_id)
            .where_and(
                field!("org_user.status"),
                CompareOp::Eq,
                ACTIVE_MEMBERSHIP_STATUS,
            )
            .where_and(field!("org_user.admin"), CompareOp::Eq, true)
            .where_and(field!("org_org.status"), CompareOp::Eq, ACTIVE_ORG_STATUS)
            .count()
            .await?;

        Ok(if admin_count > 0 {
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
