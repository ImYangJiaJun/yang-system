//! 平台账号 Action 清单。

mod add;
mod bootstrap;
mod list;
mod set_admin;
mod set_status;

use super::service::AdminService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;

pub(super) fn register_all(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    let module = bootstrap::register(module, Arc::clone(&service));
    let module = list::register(module, Arc::clone(&service));
    let module = add::register(module, Arc::clone(&service));
    let module = set_status::register(module, Arc::clone(&service));
    set_admin::register(module, service)
}
