//! Token 签发时的授权快照扩展：从 `authz_grant` 读取用户直授权限。
//!
//! 只补充权限，不附加角色（决策 D4：角色仍为账号域固定的 `user`）。

use super::context::Access;
use crate::addon::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 把直授权限集合合并为稳定去重的授权快照。
pub(crate) fn grants_from_permissions<I>(permissions: I) -> AuthorizationGrants
where
    I: IntoIterator<Item = String>,
{
    let mut grants = AuthorizationGrants::default();
    for permission in permissions {
        grants = grants.permission(permission);
    }
    grants
}

/// 账号域 `GrantResolver` 的 access 实现：按用户读取直授权限。
pub(crate) struct AuthzGrantResolver {
    access: Arc<Access>,
}

impl AuthzGrantResolver {
    pub(crate) fn new(access: Arc<Access>) -> Self {
        Self { access }
    }
}

#[async_trait]
impl GrantResolver for AuthzGrantResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        let records = self
            .access
            .grants()
            .list_by_user_in_tx(ctx, transaction, user_id)
            .await?;
        Ok(grants_from_permissions(
            records.into_iter().map(|record| record.permission),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_from_permissions_are_deduplicated_and_stably_ordered() {
        let grants = grants_from_permissions([
            "access.grants.write".to_string(),
            "access.grants.read".to_string(),
            "access.grants.read".to_string(),
        ]);

        assert_eq!(
            grants.permissions().collect::<Vec<_>>(),
            ["access.grants.read", "access.grants.write"]
        );
        assert_eq!(grants.roles().count(), 0, "access 不附加角色（决策 D4）");
    }

    #[test]
    fn user_without_grants_gets_an_empty_extension() {
        let grants = grants_from_permissions(Vec::new());

        assert_eq!(grants.permissions().count(), 0);
    }
}
