//! 授权新鲜度基础设施。

mod outbox;
mod version_cache;
mod worker;

pub(crate) use version_cache::validate_deployment_name;
pub use version_cache::{AuthorizationVersionCache, CachePublishOutcome};
pub use worker::{AuthorizationOutboxBatchReport, AuthorizationOutboxWorker};
