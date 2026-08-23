//! 平台账号对 Token 授权快照的扩展。

use super::user::ACTIVE_STATUS;
use crate::addon::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::{field, table, CompareOp, Transaction};

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
        let admin: Option<(bool, Option<String>)> = transaction
            .table(table!("admin_user"))
            .field(field!("admin"))
            .field(field!("owner_key"))
            .where_and(field!("user_user"), CompareOp::Eq, user_id)
            .where_and(field!("status"), CompareOp::Eq, ACTIVE_STATUS)
            .find()
            .await?;

        // 与原 `COALESCE(owner_key = 'system-owner', FALSE)` 一致：NULL owner_key 不是最终管理员。
        Ok(admin
            .map(|(super_admin, owner_key)| {
                grants_for_admin(super_admin, owner_key.as_deref() == Some("system-owner"))
            })
            .unwrap_or_default())
    }
}

fn grants_for_admin(super_admin: bool, system_owner: bool) -> AuthorizationGrants {
    let grants = AuthorizationGrants::default()
        .role("admin_user")
        .permission("admin.user:read");
    let grants = if super_admin {
        grants.role("admin").permission("admin.user:write")
    } else {
        grants
    };
    if system_owner {
        grants.role("system_owner")
    } else {
        grants
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_super_admin_receives_platform_write_permission() {
        let member = grants_for_admin(false, false);
        assert_eq!(member.roles().collect::<Vec<_>>(), ["admin_user"]);
        assert_eq!(
            member.permissions().collect::<Vec<_>>(),
            ["admin.user:read"]
        );

        let admin = grants_for_admin(true, false);
        assert_eq!(admin.roles().collect::<Vec<_>>(), ["admin", "admin_user"]);
        assert_eq!(
            admin.permissions().collect::<Vec<_>>(),
            ["admin.user:read", "admin.user:write"]
        );

        let owner = grants_for_admin(true, true);
        assert_eq!(
            owner.roles().collect::<Vec<_>>(),
            ["admin", "admin_user", "system_owner"]
        );
    }
}
