//! 授权新鲜度基础设施。

mod outbox;
mod request_validator;
#[cfg(any(feature = "admin", feature = "org"))]
mod resource_probe;
mod step_up;
mod version_cache;
mod worker;

pub use request_validator::AuthorizationVersionValidator;
#[cfg(any(feature = "admin", feature = "org"))]
pub(crate) use resource_probe::checkpoint as resource_authorization_checkpoint;
#[cfg(any(feature = "admin", feature = "org"))]
pub use resource_probe::{ResourceAuthorizationCheckpoint, ResourceAuthorizationProbe};
pub(crate) use step_up::{audit_result_for_error, RequestFingerprintResolver, StepUpServices};
pub(crate) use version_cache::validate_deployment_name;
pub use version_cache::{
    AuthorizationVersionCache, CachePublishOutcome, CachedAuthorizationVersion,
};
pub use worker::{AuthorizationOutboxBatchReport, AuthorizationOutboxWorker};
