//! `org.tenant` Action 清单。

mod create;
mod list;

use super::service::TenantService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<TenantService>,
) -> Result<ModuleSpec, BaseError> {
    let module = create::register(module, Arc::clone(&service))?;
    list::register(module, service)
}
