//! 用户 Action 清单。
//!
//! 所有 Action 只在这里进入 `ModuleSpec`，新增接口时无需在多层字符串路由表中重复登记。

mod login;
mod logout;
mod me;
mod refresh;
mod register;

use super::service::UserService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

/// 按清晰、可审查的顺序注册用户领域全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<UserService>,
) -> Result<ModuleSpec, BaseError> {
    let module = register::register(module, Arc::clone(&service))?;
    let module = login::register(module, Arc::clone(&service))?;
    let module = refresh::register(module, Arc::clone(&service))?;
    let module = logout::register(module)?;
    me::register(module, service)
}
