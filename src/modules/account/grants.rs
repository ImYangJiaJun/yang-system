//! 账号身份签发 Token 时使用的授权快照扩展点。

use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

/// 一次 Token 签发所需的角色与权限快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorizationGrants {
    roles: BTreeSet<String>,
    permissions: BTreeSet<String>,
}

impl AuthorizationGrants {
    /// 创建基础用户授权。
    pub fn user() -> Self {
        Self::default()
            .role("user")
            .permission("org.org:read")
            .permission("org.user:read")
    }

    /// 增加一个角色。
    #[must_use]
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.roles.insert(role.into());
        self
    }

    /// 增加一个权限。
    #[must_use]
    pub fn permission(mut self, permission: impl Into<String>) -> Self {
        self.permissions.insert(permission.into());
        self
    }

    /// 合并另一份授权快照。
    #[must_use]
    pub fn extend(mut self, other: Self) -> Self {
        self.roles.extend(other.roles);
        self.permissions.extend(other.permissions);
        self
    }

    pub(crate) fn roles(&self) -> impl Iterator<Item = &str> {
        self.roles.iter().map(String::as_str)
    }

    pub(crate) fn permissions(&self) -> impl Iterator<Item = &str> {
        self.permissions.iter().map(String::as_str)
    }
}

/// 从账号外围领域解析附加角色与权限。
#[async_trait]
pub trait GrantResolver: Send + Sync + 'static {
    /// 按用户 ID 返回附加授权；基础 `user` 授权由账号域统一补齐。
    async fn resolve(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError>;
}

/// 依次执行多个外围领域解析器，并合并为一份稳定、去重的授权快照。
pub(crate) struct CompositeGrantResolver {
    resolvers: Vec<Arc<dyn GrantResolver>>,
}

impl CompositeGrantResolver {
    pub(crate) fn new(resolvers: Vec<Arc<dyn GrantResolver>>) -> Self {
        Self { resolvers }
    }
}

#[async_trait]
impl GrantResolver for CompositeGrantResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        let mut grants = AuthorizationGrants::default();
        for resolver in &self.resolvers {
            grants = grants.extend(resolver.resolve(ctx, user_id, transaction).await?);
        }
        Ok(grants)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_are_deduplicated_and_stably_ordered() {
        let grants = AuthorizationGrants::user()
            .role("admin")
            .role("user")
            .permission("admin.user:read")
            .permission("org.user:read");

        assert_eq!(grants.roles().collect::<Vec<_>>(), ["admin", "user"]);
        assert_eq!(
            grants.permissions().collect::<Vec<_>>(),
            ["admin.user:read", "org.org:read", "org.user:read"]
        );
    }
}
