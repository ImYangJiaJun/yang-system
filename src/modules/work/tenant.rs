//! 将已认证用户解析为不可伪造的个人工作区租户。

use async_trait::async_trait;
use yang_base::action::{ActionContext, TenantContext, TenantId, TenantResolution, TenantResolver};
use yang_base::BaseError;

#[derive(Debug, Default)]
pub(super) struct PersonalWorkspaceResolver;

#[async_trait]
impl TenantResolver for PersonalWorkspaceResolver {
    async fn resolve(
        &self,
        context: &ActionContext,
        requested: Option<TenantId>,
    ) -> Result<TenantResolution, BaseError> {
        let user_id = context.actor()?.user_id();
        resolve_workspace(user_id, requested)
    }
}

fn resolve_workspace(
    user_id: i64,
    requested: Option<TenantId>,
) -> Result<TenantResolution, BaseError> {
    let workspace = TenantId::new(user_id);
    if requested.is_some_and(|requested| requested != workspace) {
        return Err(BaseError::PermissionDenied(
            "个人工作区不接受其他租户标识".to_string(),
        ));
    }
    Ok(TenantContext::new(workspace).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_workspace_uses_actor_and_rejects_forged_tenant() {
        assert!(matches!(
            resolve_workspace(7, None),
            Ok(TenantResolution::Tenant(tenant)) if tenant.id() == TenantId::new(7)
        ));
        assert!(matches!(
            resolve_workspace(7, Some(TenantId::new(8))),
            Err(BaseError::PermissionDenied(_))
        ));
    }
}
