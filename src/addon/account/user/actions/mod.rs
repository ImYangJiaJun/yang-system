//! 用户 Action 注册表。
//!
//! 每个 Action 的输入、路由、权限与业务用例都自包含在同名文件中；
//! 这里只有模块清单和注册表数组，新增接口时加 `mod` 声明和数组一行即可。

mod change_password;
mod disable_self;
mod login;
mod logout;
mod me;
mod refresh;
mod register;
mod request_registration_email;
mod reset_password;
mod step_up;

use crate::addon::account::Account;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;

/// 注册函数的统一签名。
type Register = fn(ModuleSpec, Arc<Account>) -> ModuleSpec;

/// 把模块名展开为它的自包含注册函数；数组每一行就是一个接口。
macro_rules! action_registry {
    ($($action:ident),* $(,)?) => {
        &[$($action::register as Register),*]
    };
}

/// account.user 的全部 Action，按可审查的顺序排列。
const ACTIONS: &[Register] = action_registry![
    request_registration_email,
    register,
    login,
    refresh,
    change_password, // 发布开关：credential_mutations_enabled
    disable_self,    // 发布开关：credential_mutations_enabled
    reset_password,  // 发布开关：credential_mutations_enabled
    logout,
    step_up, // 条件：组合根配置了 StepUpManager
    me,
    // scaffold:action-registration
];

/// 按注册表顺序挂载全部 Action。
pub(super) fn register_all(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    ACTIONS.iter().fold(module, |module, register| {
        register(module, Arc::clone(&account))
    })
}
