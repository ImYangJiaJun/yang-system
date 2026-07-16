use super::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{CredentialVerifier, LoginAction, LoginInput, VerifiedSubject};
use yang_base::action::ActionContext;
use yang_base::router::{ModuleRouter, RouteDescriptor};
use yang_base::BaseError;

#[derive(Clone)]
struct UserCredentialVerifier {
    service: Arc<UserService>,
}

#[async_trait]
impl CredentialVerifier for UserCredentialVerifier {
    async fn verify(
        &self,
        _ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let user = self
            .service
            .authenticate(&input.username, &input.password)
            .await?;
        Ok(
            VerifiedSubject::new(user.id.to_string()).with_claims(serde_json::json!({
                "username": user.username,
                "roles": ["user"]
            })),
        )
    }
}

pub(super) fn register(
    router: ModuleRouter,
    service: Arc<UserService>,
) -> Result<ModuleRouter, BaseError> {
    let route = RouteDescriptor::new("POST", "/api/v1/users/login", "users.login")?
        .with_success_status(200)?
        .with_tags(vec!["users".to_string()])?;
    router
        .register_action(LoginAction::new(UserCredentialVerifier { service }))?
        .register_route("login", route)
}
