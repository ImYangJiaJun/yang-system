use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{RefreshAction, RefreshClaimsResolver};
use yang_base::action::ActionContext;
use yang_base::definition::{ActionName, ActionSpec, HttpMethod, ModuleSpec, RouteSpec};
use yang_base::BaseError;

#[derive(Clone)]
struct UserClaimsResolver {
    service: Arc<UserService>,
}

#[async_trait]
impl RefreshClaimsResolver for UserClaimsResolver {
    async fn resolve(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<serde_json::Value, BaseError> {
        let user = self.service.active_user_by_subject(ctx, subject).await?;
        self.service.claims_for(ctx, &user).await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    let name =
        ActionName::new("refresh").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let spec = ActionSpec::new(
        name,
        RouteSpec::new(
            HttpMethod::Post,
            "/api/v1/users/refresh",
            "account.user.refresh",
        ),
    )
    .display_name("刷新 Token")
    .description("轮换 Refresh Token 并签发新 Token 对")
    .public(true)
    .tag("users");
    Ok(module.action(spec, RefreshAction::new(UserClaimsResolver { service })))
}
