//! 授权新鲜度基础设施。

mod outbox;
mod request_validator;
mod version_cache;
mod worker;

pub use request_validator::AuthorizationVersionValidator;
pub(crate) use version_cache::validate_deployment_name;
pub use version_cache::{
    AuthorizationVersionCache, CachePublishOutcome, CachedAuthorizationVersion,
};
pub use worker::{AuthorizationOutboxBatchReport, AuthorizationOutboxWorker};
