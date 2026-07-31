//! 应用级 Step-up 运行时与资源指纹。

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[cfg(test)]
use yang_base::action::InMemoryStepUpProofStore;
use yang_base::action::{
    ActionContext, RedisStepUpProofStore, StepUpManager, StepUpMiddleware, StepUpProofStore,
    StepUpResourceResolver, StepUpVerification,
};
use yang_base::definition::ActionRef;
use yang_base::BaseError;

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

    pub(crate) fn middleware<R>(&self, target: ActionRef, resolver: R) -> StepUpMiddleware<R>
    where
        R: StepUpResourceResolver,
    {
        StepUpMiddleware::new(Arc::clone(&self.manager), target, resolver)
            .with_proof_store(self.proof_store.clone())
    }
}

#[derive(Debug, Clone, Copy)]
enum ResourceScope {
    Global,
    Tenant,
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

    pub(crate) const fn tenant(namespace: &'static str) -> Self {
        Self {
            namespace,
            scope: ResourceScope::Tenant,
        }
    }
}

#[async_trait]
impl StepUpResourceResolver for RequestFingerprintResolver {
    async fn resolve(&self, context: &ActionContext) -> Result<String, BaseError> {
        let scope = match self.scope {
            ResourceScope::Global => "global".to_string(),
            ResourceScope::Tenant => match context.tenant() {
                Ok(tenant) => format!("tenant={}", tenant.id().get()),
                Err(_) => {
                    let user = context.authenticated_user().ok_or_else(|| {
                        BaseError::Unauthorized("Step-up 资源解析需要已认证用户".to_string())
                    })?;
                    let capability = context.system_tenant()?;
                    if capability.actor().user_id() != user.id {
                        return Err(BaseError::PermissionDenied(
                            "系统租户 capability 与 Step-up 操作者不匹配".to_string(),
                        ));
                    }
                    "system".to_string()
                }
            },
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
    use yang_base::action::{Request, TenantContext, TenantId};
    use yang_base::tools::ToolsBuilder;

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

    #[tokio::test]
    async fn tenant_fingerprint_uses_trusted_context_not_client_body() {
        let resolver = RequestFingerprintResolver::tenant("org-user");
        let first = context(serde_json::json!({ "id": 9, "org_org": 999 }))
            .with_tenant(TenantContext::new(TenantId::new(7)));
        let second = context(serde_json::json!({ "id": 9, "org_org": 999 }))
            .with_tenant(TenantContext::new(TenantId::new(8)));

        let first = resolver
            .resolve(&first)
            .await
            .unwrap_or_else(|error| panic!("租户资源指纹应生成: {error}"));
        let second = resolver
            .resolve(&second)
            .await
            .unwrap_or_else(|error| panic!("租户资源指纹应生成: {error}"));
        assert_ne!(first, second);
        assert!(first.contains("tenant=7"));
        assert!(!first.contains("999"));
    }
}
