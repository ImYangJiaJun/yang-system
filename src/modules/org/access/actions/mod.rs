//! `org.tenant` Action 清单。

mod list;

use super::service::TenantService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<TenantService>,
) -> Result<ModuleSpec, BaseError> {
    list::register(module, service)
}
