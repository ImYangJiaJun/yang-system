//! 应用级 Step-up 运行时与资源指纹。

use crate::audit::{self, AuditActor, AuditEntity, AuditEvent, AuditEventContext, AuditResult};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[cfg(test)]
use yang_base::action::InMemoryStepUpProofStore;
use yang_base::action::{
    ActionContext, ApiResponse, RedisStepUpProofStore, StepUpManager, StepUpProofStore,
    StepUpResourceResolver, StepUpVerification, STEP_UP_PROOF_HEADER,
};
use yang_base::definition::ActionRef;
use yang_base::router::{Middleware, MiddlewareRole, Next};
use yang_base::{BaseError, ErrorCategory};

#[derive(Clone)]
enum ApplicationProofStore {
    Redis(RedisStepUpProofStore),
    #[cfg(test)]
    Memory(Arc<InMemoryStepUpProofStore>),
}

#[async_trait]
impl StepUpProofStore for ApplicationProofStore {
    async fn consume(&self, proof: &StepUpVerification) -> Result<bool, BaseError> {
        match self {
            Self::Redis(store) => store.consume(proof).await,
            #[cfg(test)]
            Self::Memory(store) => store.consume(proof).await,
        }
    }
}

#[derive(Clone)]
pub struct StepUpServices {
    manager: Arc<StepUpManager>,
    proof_store: ApplicationProofStore,
}

impl StepUpServices {
    pub(crate) fn production(
        manager: Arc<StepUpManager>,
        cache: yang_db::RedisClient,
    ) -> Result<Self, BaseError> {
        let proof_store =
            RedisStepUpProofStore::new(cache).with_key_prefix("yang-system:step-up:proof-used:")?;
        Ok(Self {
            manager,
            proof_store: ApplicationProofStore::Redis(proof_store),
        })
    }

    #[cfg(test)]
    pub(crate) fn in_memory(manager: Arc<StepUpManager>) -> Self {
        Self {
            manager,
            proof_store: ApplicationProofStore::Memory(Arc::new(
                InMemoryStepUpProofStore::default(),
            )),
        }
    }

    pub(crate) fn manager(&self) -> Arc<StepUpManager> {
        Arc::clone(&self.manager)
    }

    pub(crate) fn middleware(
        &self,
        target: ActionRef,
        resolver: RequestFingerprintResolver,
    ) -> AuditedStepUpMiddleware {
        AuditedStepUpMiddleware {
            manager: Arc::clone(&self.manager),
            action: target,
            resolver,
            proof_store: self.proof_store.clone(),
        }
    }
}

/// 把 proof 验证结果与后续高危 Action 拒绝/失败写入独立持久审计。
///
/// proof 接受事件必须先落库再进入业务 Action（fail-closed）；已经拒绝或失败的请求
/// 即使审计库不可用也保持原错误，避免审计故障改变安全决定。
pub(crate) struct AuditedStepUpMiddleware {
    manager: Arc<StepUpManager>,
    action: ActionRef,
    resolver: RequestFingerprintResolver,
    proof_store: ApplicationProofStore,
}

#[async_trait]
impl Middleware for AuditedStepUpMiddleware {
    fn role(&self) -> MiddlewareRole {
        MiddlewareRole::StepUpProtection
    }

    fn target_action(&self) -> Option<&ActionRef> {
        Some(&self.action)
    }

    async fn handle(
        &self,
        context: ActionContext,
        next: Next<'_>,
    ) -> Result<ApiResponse, BaseError> {
        let subject = context.actor()?.user_id();
        let resource = self.resolver.resolve(&context).await?;
        if resource.trim().is_empty() {
            return Err(BaseError::ConfigError(
                "Step-up 资源解析器返回了空标识".to_string(),
            ));
        }
        let facts = StepUpAuditFacts::from_context(&context, &self.action, resource)?;

        let Some(proof) = context.request.get_header(STEP_UP_PROOF_HEADER) else {
            let challenge = self.manager.issue_challenge(
                subject.to_string(),
                &self.action,
                facts.resource(),
            )?;
            facts
                .record_best_effort(
                    "security.step_up",
                    AuditResult::Denied,
                    "challenge_required",
                    Some("step_up_required"),
                )
                .await;
            return Err(BaseError::StepUpRequired(challenge));
        };

        let verification = match self.manager.verify_proof(
            proof,
            &subject.to_string(),
            &self.action,
            facts.resource(),
        ) {
            Ok(verification) => verification,
            Err(error) => {
                facts
                    .record_error_best_effort("security.step_up", "proof_denied", &error)
                    .await;
                return Err(error);
            }
        };
        let consumed = match self.proof_store.consume(&verification).await {
            Ok(consumed) => consumed,
            Err(error) => {
                facts
                    .record_error_best_effort("security.step_up", "proof_store_failed", &error)
                    .await;
                return Err(error);
            }
        };
        if !consumed {
            let error = BaseError::Unauthorized("Step-up proof 已被消费".to_string());
            facts
                .record_error_best_effort("security.step_up", "proof_replayed", &error)
                .await;
            return Err(error);
        }

        // proof 已被共享存储消费；审计失败时拒绝进入业务写入，不能制造未审计的高危操作。
        facts
            .record(
                "security.step_up",
                AuditResult::Succeeded,
                "proof_accepted",
                None,
            )
            .await?;

        let result = next.run(context).await;
        if let Err(error) = &result {
            facts
                .record_error_best_effort(facts.target_action(), "action_rejected", error)
                .await;
        }
        result
    }
}

struct StepUpAuditFacts {
    pool: sqlx::MySqlPool,
    context: AuditEventContext,
    subject: AuditEntity,
    target: AuditEntity,
    target_action: String,
    resource: String,
}

impl StepUpAuditFacts {
    fn from_context(
        context: &ActionContext,
        target_action: &ActionRef,
        resource: String,
    ) -> Result<Self, BaseError> {
        let actor_id = context.actor()?.user_id();
        let tenant_id = context.tenant().ok().map(|tenant| tenant.id().get());
        let audit_context = AuditEventContext::new(
            AuditActor::user(actor_id).map_err(invalid_audit_event)?,
            tenant_id,
            context.request_id(),
        )
        .map_err(invalid_audit_event)?;
        Ok(Self {
            pool: context.tools().mysql()?.pool().clone(),
            context: audit_context,
            subject: AuditEntity::new("user", actor_id.to_string()).map_err(invalid_audit_event)?,
            target: AuditEntity::new("step_up_resource", resource.clone())
                .map_err(invalid_audit_event)?,
            target_action: target_action.to_string(),
            resource,
        })
    }

    fn resource(&self) -> &str {
        &self.resource
    }

    fn target_action(&self) -> &str {
        &self.target_action
    }

    async fn record(
        &self,
        action: &str,
        result: AuditResult,
        outcome_code: &'static str,
        error_code: Option<&str>,
    ) -> Result<(), BaseError> {
        let mut fields = vec![
            ("outcome_code", serde_json::json!(outcome_code)),
            ("target_action", serde_json::json!(self.target_action)),
        ];
        if let Some(error_code) = error_code {
            fields.push(("error_code", serde_json::json!(error_code)));
        }
        let event = AuditEvent::new(
            self.context.clone(),
            action,
            Some(self.subject.clone()),
            self.target.clone(),
            result,
            None,
            Some(audit::AuditSummary::try_from_fields(fields).map_err(invalid_audit_event)?),
        )
        .map_err(invalid_audit_event)?;
        audit::append_independent(&self.pool, &event).await
    }

    async fn record_best_effort(
        &self,
        action: &str,
        result: AuditResult,
        outcome_code: &'static str,
        error_code: Option<&str>,
    ) {
        if let Err(audit_error) = self.record(action, result, outcome_code, error_code).await {
            tracing::error!(
                target_action = %self.target_action,
                result = result.as_str(),
                audit_error_code = audit_error.code_str(),
                "Step-up 拒绝或失败事件持久化失败，保留原安全结果"
            );
        }
    }

    async fn record_error_best_effort(
        &self,
        action: &str,
        outcome_code: &'static str,
        error: &BaseError,
    ) {
        self.record_best_effort(
            action,
            audit_result_for_error(error),
            outcome_code,
            Some(error.code_str()),
        )
        .await;
    }
}

pub(crate) fn audit_result_for_error(error: &BaseError) -> AuditResult {
    match error.category() {
        ErrorCategory::Auth | ErrorCategory::Client | ErrorCategory::NotFound => {
            AuditResult::Denied
        }
        ErrorCategory::Conflict | ErrorCategory::Transient | ErrorCategory::Server => {
            AuditResult::Failed
        }
        _ => AuditResult::Failed,
    }
}

fn invalid_audit_event(error: anyhow::Error) -> BaseError {
    BaseError::ConfigError(format!("构建 Step-up 审计事件失败: {error}"))
}

#[derive(Debug, Clone, Copy)]
enum ResourceScope {
    Global,
}

#[derive(Debug, Clone)]
pub(crate) struct RequestFingerprintResolver {
    namespace: &'static str,
    scope: ResourceScope,
}

impl RequestFingerprintResolver {
    pub(crate) const fn global(namespace: &'static str) -> Self {
        Self {
            namespace,
            scope: ResourceScope::Global,
        }
    }
}

#[async_trait]
impl StepUpResourceResolver for RequestFingerprintResolver {
    async fn resolve(&self, context: &ActionContext) -> Result<String, BaseError> {
        let scope = match self.scope {
            ResourceScope::Global => "global".to_string(),
        };
        let canonical = canonical_json(&context.request.body)?;
        let digest = Sha256::digest(canonical.as_bytes());
        Ok(format!("{}:{scope}:sha256:{digest:x}", self.namespace))
    }
}

fn canonical_json(value: &Value) -> Result<String, BaseError> {
    fn normalize(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let mut entries = object.iter().collect::<Vec<_>>();
                entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key.clone(), normalize(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.iter().map(normalize).collect()),
            scalar => scalar.clone(),
        }
    }

    serde_json::to_string(&normalize(value))
        .map_err(|error| BaseError::JsonSerializeFailed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::action::Request;
    use yang_base::tools::ToolsBuilder;

    fn manager() -> Arc<StepUpManager> {
        Arc::new(
            StepUpManager::new(
                "independent-step-up-audit-test-secret-0123456789abcdef",
                "test-step-up-audit",
                "test-sensitive-actions",
            )
            .unwrap_or_else(|error| panic!("测试 Step-up manager 应有效: {error}")),
        )
    }

    fn context(body: Value) -> ActionContext {
        ActionContext::new(
            Request::new(body),
            Arc::new(
                ToolsBuilder::new()
                    .build()
                    .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
            ),
        )
    }

    #[tokio::test]
    async fn canonical_fingerprint_binds_every_nested_semantic_value() {
        let resolver = RequestFingerprintResolver::global("admin-user");
        let left = resolver
            .resolve(&context(serde_json::json!({
                "id": 7,
                "data": { "admin": true, "status": "active" }
            })))
            .await
            .unwrap_or_else(|error| panic!("资源指纹应生成: {error}"));
        let reordered = resolver
            .resolve(&context(serde_json::json!({
                "data": { "status": "active", "admin": true },
                "id": 7
            })))
            .await
            .unwrap_or_else(|error| panic!("重排字段后资源指纹应生成: {error}"));
        let changed = resolver
            .resolve(&context(serde_json::json!({
                "id": 7,
                "data": { "admin": false, "status": "active" }
            })))
            .await
            .unwrap_or_else(|error| panic!("变更字段后资源指纹应生成: {error}"));

        assert_eq!(left, reordered);
        assert_ne!(left, changed);
        assert!(!left.contains("active"));
    }

    #[test]
    fn audited_middleware_is_bound_to_one_target_and_security_role() {
        let services = StepUpServices::in_memory(manager());
        let target = yang_base::action!("admin.user.set_admin");
        let middleware = services.middleware(
            target.clone(),
            RequestFingerprintResolver::global("admin-user"),
        );

        assert_eq!(middleware.target_action(), Some(&target));
        assert_eq!(middleware.role(), MiddlewareRole::StepUpProtection);
    }

    #[test]
    fn audit_classification_separates_refusals_from_infrastructure_failures() {
        assert_eq!(
            audit_result_for_error(&BaseError::Unauthorized("bad proof".to_string())),
            AuditResult::Denied
        );
        assert_eq!(
            audit_result_for_error(&BaseError::PermissionDenied("forbidden".to_string())),
            AuditResult::Denied
        );
        assert_eq!(
            audit_result_for_error(&BaseError::RedisNotInitialized),
            AuditResult::Failed
        );
        assert_eq!(
            audit_result_for_error(&BaseError::Unknown("boom".to_string())),
            AuditResult::Failed
        );
    }
}
