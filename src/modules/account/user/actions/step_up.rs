use super::super::service::UserService;
use crate::audit::{self, AuditActor, AuditEntity, AuditEvent, AuditEventContext, AuditResult};
use crate::security::audit_result_for_error;
use async_trait::async_trait;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use yang_base::action::auth::{CredentialVerifier, LoginInput, VerifiedSubject};
use yang_base::action::{Action as BusinessAction, ActionContext, StepUpManager, StepUpProof};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Clone)]
struct UserStepUpCredentialVerifier {
    service: Arc<UserService>,
    verified_user_id: Arc<AtomicI64>,
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
        self.verified_user_id.store(user.id, Ordering::Release);
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
    manager: Arc<StepUpManager>,
    service: Arc<UserService>,
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
        let verified_user_id = Arc::new(AtomicI64::new(0));
        let verifier = UserStepUpCredentialVerifier {
            service: Arc::clone(&self.service),
            verified_user_id: Arc::clone(&verified_user_id),
        };
        let result = async {
            super::super::browser_session::validate_same_origin(&ctx.request)?;
            self.manager
                .complete_challenge(
                    &ctx,
                    &verifier,
                    &LoginInput {
                        username: input.credentials.username,
                        password: input.credentials.password,
                        extra: serde_json::Value::Null,
                    },
                    &input.challenge,
                )
                .await
        }
        .await;
        let verified_user_id = verified_user_id.load(Ordering::Acquire);
        let event = completion_audit_event(&ctx, verified_user_id, &result)?;
        match result {
            Ok(proof) => {
                // proof 尚未返回给客户端；成功审计不可用时失败关闭且不泄露 proof。
                audit::append_independent(ctx.tools().mysql()?.pool(), &event).await?;
                Ok(proof)
            }
            Err(error) => {
                if let Err(audit_error) =
                    audit::append_independent(ctx.tools().mysql()?.pool(), &event).await
                {
                    tracing::error!(
                        error_code = error.code_str(),
                        audit_error_code = audit_error.code_str(),
                        "Step-up 凭据复核拒绝或失败事件持久化失败，保留原安全结果"
                    );
                }
                Err(error)
            }
        }
    }
}

fn completion_audit_event(
    ctx: &ActionContext,
    verified_user_id: i64,
    result: &Result<StepUpProof, BaseError>,
) -> Result<AuditEvent, BaseError> {
    let trusted_actor_id = ctx
        .authenticated_user()
        .map(|user| user.id)
        .filter(|user_id| *user_id > 0);
    let actor = match trusted_actor_id.or((verified_user_id > 0).then_some(verified_user_id)) {
        Some(user_id) => AuditActor::user(user_id),
        None => AuditActor::system("step-up-verifier"),
    }
    .map_err(invalid_audit_event)?;
    let context =
        AuditEventContext::new(actor, None, ctx.request_id()).map_err(invalid_audit_event)?;
    let (audit_result, outcome_code, error_code) = match result {
        Ok(_) => (AuditResult::Succeeded, "credentials_accepted", None),
        Err(error) => (
            audit_result_for_error(error),
            "credentials_rejected",
            Some(error.code_str()),
        ),
    };
    let mut fields = vec![("outcome_code", serde_json::json!(outcome_code))];
    if let Some(error_code) = error_code {
        fields.push(("error_code", serde_json::json!(error_code)));
    }
    let subject = (verified_user_id > 0)
        .then(|| AuditEntity::new("user", verified_user_id.to_string()))
        .transpose()
        .map_err(invalid_audit_event)?;
    let action = ctx
        .dispatch_target()
        .map(|(module, action)| format!("{module}.{action}"))
        .ok_or_else(|| {
            BaseError::ConfigError("Step-up 完成 Action 缺少可信派发目标".to_string())
        })?;
    AuditEvent::new(
        context,
        action,
        subject,
        AuditEntity::new("step_up_completion", ctx.request_id().to_string())
            .map_err(invalid_audit_event)?,
        audit_result,
        None,
        Some(audit::AuditSummary::try_from_fields(fields).map_err(invalid_audit_event)?),
    )
    .map_err(invalid_audit_event)
}

fn invalid_audit_event(error: anyhow::Error) -> BaseError {
    BaseError::ConfigError(format!("构建 Step-up 完成审计事件失败: {error}"))
}

pub(super) fn register(
    module: ModuleSpec,
    service: Arc<UserService>,
    manager: Arc<StepUpManager>,
) -> ModuleSpec {
    module.native_action(BrowserStepUpCompleteAction { manager, service })
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
