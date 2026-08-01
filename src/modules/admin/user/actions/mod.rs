//! 平台账号 Action 清单。

mod add;
mod create_password_reset;
mod list;
mod set_admin;
mod set_status;

use super::service::AdminService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;

pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<AdminService>,
    password_reset_enabled: bool,
) -> ModuleSpec {
    let module = list::register(module, Arc::clone(&service));
    let module = add::register(module, Arc::clone(&service));
    let module = if password_reset_enabled {
        create_password_reset::register(module, Arc::clone(&service))
    } else {
        module
    };
    let module = set_status::register(module, Arc::clone(&service));
    set_admin::register(module, service)
}
