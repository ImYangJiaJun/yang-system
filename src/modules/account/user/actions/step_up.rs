use super::super::service::UserService;
use async_trait::async_trait;
use std::sync::Arc;
use yang_base::action::auth::{CredentialVerifier, LoginInput, VerifiedSubject};
use yang_base::action::{
    Action as BusinessAction, ActionContext, StepUpCompleteAction, StepUpCompleteInput,
    StepUpManager, StepUpProof, TypedHandler,
};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Clone)]
struct UserStepUpCredentialVerifier {
    service: Arc<UserService>,
}

#[async_trait]
impl CredentialVerifier for UserStepUpCredentialVerifier {
    async fn verify(
        &self,
        ctx: &ActionContext,
        input: &LoginInput,
    ) -> Result<VerifiedSubject, BaseError> {
        let user = self
            .service
            .authenticate_step_up(ctx, &input.username, &input.password)
            .await?;
        Ok(VerifiedSubject::new(user.id.to_string()))
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompleteStepUpInput {
    challenge: String,
    credentials: CompleteStepUpCredentials,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompleteStepUpCredentials {
    username: String,
    password: String,
}

impl ParamInput for CompleteStepUpInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(yang_base::Action)]
#[action(
    name = "step_up_complete",
    display_name = "完成敏感操作重认证",
    description = "重新校验账号密码并把短期 challenge 升级为一次性 proof",
    method = "POST",
    path = "/api/v1/users/step-up/complete",
    public
)]
struct BrowserStepUpCompleteAction {
    inner: StepUpCompleteAction<UserStepUpCredentialVerifier>,
}

#[async_trait]
impl BusinessAction for BrowserStepUpCompleteAction {
    type Input = CompleteStepUpInput;
    type Output = StepUpProof;

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        super::super::browser_session::validate_same_origin(&ctx.request)?;
        self.inner
            .handle(
                ctx,
                StepUpCompleteInput {
                    challenge: input.challenge,
                    credentials: LoginInput {
                        username: input.credentials.username,
                        password: input.credentials.password,
                        extra: serde_json::Value::Null,
                    },
                },
            )
            .await
    }
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
    manager: Arc<StepUpManager>,
) -> ModuleSpec {
    module.native_action(BrowserStepUpCompleteAction {
        inner: StepUpCompleteAction::new(manager, UserStepUpCredentialVerifier { service }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_contract_rejects_unknown_and_client_claim_fields() {
        for raw in [
            serde_json::json!({
                "challenge": "signed",
                "credentials": { "username": "alice", "password": "secret", "subject": "7" }
            }),
            serde_json::json!({
                "challenge": "signed",
                "credentials": { "username": "alice", "password": "secret" },
                "action": "admin.user.set_admin"
            }),
        ] {
            assert!(serde_json::from_value::<CompleteStepUpInput>(raw).is_err());
        }
    }
}
