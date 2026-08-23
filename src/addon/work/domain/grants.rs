//! 个人任务规划对基础用户授权快照的扩展。

use crate::addon::account::{AuthorizationGrants, GrantResolver};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::BaseError;
use yang_db::Transaction;

#[derive(Debug, Default)]
struct WorkGrantResolver;

#[async_trait]
impl GrantResolver for WorkGrantResolver {
    async fn resolve(
        &self,
        _ctx: &ActionContext,
        _user_id: i64,
        _transaction: &mut Transaction,
    ) -> Result<AuthorizationGrants, BaseError> {
        Ok(work_grants())
    }
}

pub(crate) fn grant_resolver() -> Arc<dyn GrantResolver> {
    Arc::new(WorkGrantResolver)
}

fn work_grants() -> AuthorizationGrants {
    AuthorizationGrants::default()
        .permission("work.project:read")
        .permission("work.project:write")
        .permission("work.task:read")
        .permission("work.task:write")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_authenticated_user_receives_personal_workspace_permissions() {
        assert_eq!(
            work_grants().permissions().collect::<Vec<_>>(),
            [
                "work.project:read",
                "work.project:write",
                "work.task:read",
                "work.task:write"
            ]
        );
    }
}
