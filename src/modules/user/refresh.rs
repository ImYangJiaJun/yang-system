use super::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{RefreshAction, RefreshClaimsResolver};
use yang_base::action::ActionContext;
use yang_base::router::{ModuleRouter, RouteDescriptor};
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
        Ok(serde_json::json!({
            "username": user.username,
            "roles": ["user"]
        }))
    }
}

pub(super) fn register(
    router: ModuleRouter,
    service: Arc<UserService>,
) -> Result<ModuleRouter, BaseError> {
    let route = RouteDescriptor::new("POST", "/api/v1/users/refresh", "users.refresh")?
        .with_success_status(200)?
        .with_tags(vec!["users".to_string()])?;
    router
        .register_action(RefreshAction::new(UserClaimsResolver { service }))?
        .register_route("refresh", route)
}
