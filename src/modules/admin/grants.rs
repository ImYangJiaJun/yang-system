//! 平台账号对 Token 授权快照的扩展。

use super::user::{ACTIVE_STATUS, IS_ADMIN, STATUS, USER_ID};
use crate::modules::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

#[derive(Debug, Default)]
pub(super) struct AdminGrantResolver;

#[async_trait]
impl GrantResolver for AdminGrantResolver {
    async fn resolve(
        &self,
        _ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        let sql = format!(
            "SELECT `{IS_ADMIN}` FROM `admin_user` \
             WHERE `{USER_ID}` = ? AND `{STATUS}` = ? LIMIT 1"
        );
        let executor = transaction.executor().ok_or_else(|| {
            BaseError::from(yang_db::DbError::TransactionError(
                "授权快照事务已结束".to_string(),
            ))
        })?;
        let admin = sqlx::query_scalar::<_, bool>(&sql)
            .bind(user_id)
            .bind(ACTIVE_STATUS)
            .fetch_optional(executor)
            .await
            .map_err(yang_db::DbError::from)?;

        Ok(admin.map(grants_for_admin).unwrap_or_default())
    }
}

fn grants_for_admin(super_admin: bool) -> AuthorizationGrants {
    let grants = AuthorizationGrants::default()
        .role("admin_user")
        .permission("admin.user:read");
    if super_admin {
        grants.role("admin").permission("admin.user:write")
    } else {
        grants
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_super_admin_receives_platform_write_permission() {
        let member = grants_for_admin(false);
        assert_eq!(member.roles().collect::<Vec<_>>(), ["admin_user"]);
        assert_eq!(
            member.permissions().collect::<Vec<_>>(),
            ["admin.user:read"]
        );

        let admin = grants_for_admin(true);
        assert_eq!(admin.roles().collect::<Vec<_>>(), ["admin", "admin_user"]);
        assert_eq!(
            admin.permissions().collect::<Vec<_>>(),
            ["admin.user:read", "admin.user:write"]
        );
    }
}
