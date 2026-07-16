use super::{UserService, USERNAME, USER_ID};
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{CredentialVerifier, LoginAction, LoginInput, VerifiedSubject};
use yang_base::action::ActionContext;
use yang_base::router::Api;
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
        let id: i64 = user.require(USER_ID)?;
        let username: String = user.require(USERNAME)?;
        Ok(
            VerifiedSubject::new(id.to_string()).with_claims(serde_json::json!({
                "username": username,
                "roles": ["user"]
            })),
        )
    }
}

pub(super) fn api(service: Arc<UserService>) -> Api {
    Api::post(
        "/api/v1/users/login",
        LoginAction::new(UserCredentialVerifier { service }),
    )
    .operation_id("users.login")
    .tag("users")
}
