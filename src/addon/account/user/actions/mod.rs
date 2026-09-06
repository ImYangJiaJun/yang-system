//! 用户 Action 注册表。
//!
//! 每个 Action 的输入、路由、权限与业务用例都自包含在同名文件中；
//! 这里只有模块清单和注册表数组，新增接口时加 `mod` 声明和数组一行即可。

mod admin_disable_user;
mod admin_enable_user;
mod admin_issue_password_reset;
mod change_email;
mod change_password;
mod change_username;
mod delete_account;
mod disable_self;
mod list_sessions;
mod list_users;
mod login;
mod logout;
mod me;
mod refresh;
mod register;
mod request_change_email;
mod request_password_reset;
mod request_registration_email;
mod reset_password;
mod revoke_session;
mod security_events;
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
    list_sessions,
    list_users,
    refresh,
    admin_disable_user,
    admin_enable_user,
    admin_issue_password_reset,
    change_password, // 发布开关：credential_mutations_enabled
    change_username, // 发布开关：credential_mutations_enabled
    change_email,    // 发布开关：credential_mutations_enabled
    delete_account,  // 发布开关：credential_mutations_enabled
    disable_self,    // 发布开关：credential_mutations_enabled
    request_change_email,
    request_password_reset,
    reset_password, // 发布开关：credential_mutations_enabled
    logout,
    step_up, // 条件：组合根配置了 StepUpManager
    revoke_session,
    security_events,
    me,
    // scaffold:action-registration
];

/// 按注册表顺序挂载全部 Action。
pub(super) fn register_all(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    ACTIONS.iter().fold(module, |module, register| {
        register(module, Arc::clone(&account))
    })
}
