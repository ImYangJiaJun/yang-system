//! 用户 Action 清单。
//!
//! 所有 Action 只在这里进入 `ModuleSpec`，新增接口时无需在多层字符串路由表中重复登记。

mod change_password;
mod disable_self;
mod login;
mod logout;
mod me;
mod refresh;
mod register;
mod reset_password;
mod step_up;

use super::service::UserService;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

/// 按清晰、可审查的顺序注册用户领域全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<UserService>,
    credential_mutations_enabled: bool,
    step_up_manager: Option<Arc<yang_base::action::StepUpManager>>,
) -> Result<ModuleSpec, BaseError> {
    let module = register::register(module, Arc::clone(&service))?;
    let module = login::register(module, Arc::clone(&service))?;
    let module = refresh::register(module, Arc::clone(&service))?;
    let module = if credential_mutations_enabled {
        let module = change_password::register(module, Arc::clone(&service))?;
        let module = disable_self::register(module, Arc::clone(&service))?;
        reset_password::register(module, Arc::clone(&service))?
    } else {
        module
    };
    let module = logout::register(module, Arc::clone(&service))?;
    let module = match step_up_manager {
        Some(manager) => step_up::register(module, Arc::clone(&service), manager),
        None => module,
    };
    let module = me::register(module, service)?;
    // scaffold:action-registration
    Ok(module)
}
