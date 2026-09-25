//! Token 签发时的权限组解析：把用户所在组折叠为附加权限。
//!
//! 只补充权限，不附加角色（spec §6.1：Token 中仍只有具体权限字符串，
//! 运行期 `permissions_match` 语义不变）。

use super::context::Access;
use super::groups::resolution::{catalog_permissions, resolve_group_permissions};
use crate::addon::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 把解析出的权限集合合并为稳定去重的授权快照。
pub(crate) fn grants_from_resolved<I>(permissions: I) -> AuthorizationGrants
where
    I: IntoIterator<Item = String>,
{
    let mut grants = AuthorizationGrants::default();
    for permission in permissions {
        grants = grants.permission(permission);
    }
    grants
}

/// 账号域 `GrantResolver` 的权限组实现。
pub(crate) struct GroupGrantResolver {
    access: Arc<Access>,
}

impl GroupGrantResolver {
    pub(crate) fn new(access: Arc<Access>) -> Self {
        Self { access }
    }
}

#[async_trait]
impl GrantResolver for GroupGrantResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        // 目录未安装时 `catalog_permissions` 直接报错：组解析绝不退化成
        // 「匹配不到任何权限」，也绝不越权放行。
        let catalog = catalog_permissions(self.access.permission_catalog())?;
        let group_ids = self
            .access
            .groups()
            .list_group_ids_of_user_in_tx(ctx, transaction, user_id)
            .await?;
        let mut permissions: Vec<String> = Vec::new();
        for group_id in group_ids {
            // 成员行指向的组在同一事务内被删除时按「无组」降级，而不是签发失败。
            let group = self
                .access
                .groups()
                .find_by_id_in_tx(ctx, transaction, group_id)
                .await?;
            let Some(group) = group else { continue };
            let items = self
                .access
                .groups()
                .list_items_in_tx(ctx, transaction, group_id)
                .await?;
            permissions.extend(resolve_group_permissions(
                &group.group_key,
                &items,
                &catalog,
            ));
        }
        Ok(grants_from_resolved(permissions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_permissions_are_merged_as_permissions_only() {
        // 组名绝不进入角色集合（spec §6.1：角色仍为账号域固定的 user）。
        let grants = grants_from_resolved([
            "access.grants.read".to_string(),
            "access.grants.read".to_string(),
            "account.users.read".to_string(),
        ]);
        assert_eq!(
            grants.permissions().collect::<Vec<_>>(),
            ["access.grants.read", "account.users.read"]
        );
        assert_eq!(grants.roles().count(), 0, "组解析不得附加任何角色");
    }

    #[test]
    fn empty_membership_yields_an_empty_extension() {
        assert_eq!(grants_from_resolved(Vec::new()).permissions().count(), 0);
    }
}
