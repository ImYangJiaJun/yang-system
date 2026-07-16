use super::{UserService, USERNAME};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{RefreshAction, RefreshClaimsResolver};
use yang_base::action::ActionContext;
use yang_base::router::Api;
use yang_base::BaseError;

#[derive(Clone)]
struct UserClaimsResolver {
    service: Arc<UserService>,
}

#[async_trait]
impl RefreshClaimsResolver for UserClaimsResolver {
    async fn resolve(
        &self,
        _ctx: &ActionContext,
        subject: &str,
    ) -> Result<serde_json::Value, BaseError> {
        let user = self.service.active_by_subject(subject).await?;
        let username: String = user.require(USERNAME)?;
        Ok(serde_json::json!({
            "username": username,
            "roles": ["user"]
        }))
    }
}

pub(super) fn api(service: Arc<UserService>) -> Api {
    Api::post(
        "/api/v1/users/refresh",
        RefreshAction::new(UserClaimsResolver { service }),
    )
    .operation_id("users.refresh")
    .tag("users")
}
