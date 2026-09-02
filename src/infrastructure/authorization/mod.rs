//! 授权新鲜度基础设施。

mod outbox;
mod ports;
mod request_validator;
mod step_up;
mod version_cache;
mod worker;

pub use ports::{
    AuthorizationPort, AuthorizationVersionSnapshot, AuthorizationVersionSource,
    AuthorizationVersionWriter, LockedAuthorization,
};
pub use request_validator::AuthorizationVersionValidator;
pub(crate) use step_up::{audit_result_for_error, RequestFingerprintResolver, StepUpServices};
pub(crate) use version_cache::validate_deployment_name;
pub use version_cache::{
    AuthorizationVersionCache, CachePublishOutcome, CachedAuthorizationVersion,
};
pub use worker::{AuthorizationOutboxBatchReport, AuthorizationOutboxWorker};
