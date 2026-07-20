use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{CredentialVerifier, LoginAction, LoginInput, VerifiedSubject};
use yang_base::action::ActionContext;
use yang_base::definition::{ActionName, ActionSpec, HttpMethod, ModuleSpec, RouteSpec};
use yang_base::BaseError;

#[derive(Clone)]
struct UserCredentialVerifier {
    service: Arc<UserService>,
}

#[async_trait]
impl CredentialVerifier for UserCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let user = self
            .service
            .authenticate(ctx, &input.username, &input.password)
            .await?;
        let claims = self.service.claims_for(ctx, &user).await?;
        Ok(VerifiedSubject::new(user.id.to_string()).with_claims(claims))
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    let name =
        ActionName::new("login").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let spec = ActionSpec::new(
        name,
        RouteSpec::new(
            HttpMethod::Post,
            "/api/v1/users/login",
            "account.user.login",
        ),
    )
    .display_name("登录")
    .description("校验账号密码并签发 Token")
    .public(true)
    .tag("users");
    Ok(module.action(spec, LoginAction::new(UserCredentialVerifier { service })))
}
