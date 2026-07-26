use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{RefreshAction, RefreshClaimsResolver, TokenPairClaims};
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
        Ok(self.service.claims_for_subject(ctx, subject).await?.access)
    }

    async fn resolve_pair(
        &self,
        ctx: &ActionContext,
        subject: &str,
    ) -> Result<TokenPairClaims, BaseError> {
        self.service.claims_for_subject(ctx, subject).await
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
