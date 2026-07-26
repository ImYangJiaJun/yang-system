//! 授权新鲜度基础设施。

mod version_cache;

pub(crate) use version_cache::validate_deployment_name;
pub use version_cache::{AuthorizationVersionCache, CachePublishOutcome};
