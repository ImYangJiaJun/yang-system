//! 平台账号 Action 清单。

mod bootstrap;

use super::service::AdminService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;

pub(super) fn register_all(module: ModuleSpec, service: Arc<AdminService>) -> ModuleSpec {
    bootstrap::register(module, service)
}
