//! 后台用户管理 Action 注册表。
//!
//! 每个 Action 的输入、路由、权限与业务用例都自包含在同名文件中；
//! 这里只有模块清单和注册表数组，新增接口时加 `mod` 声明和数组一行即可。

mod admin_disable_user;
mod admin_enable_user;
mod admin_issue_password_reset;
mod list_users;

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

/// account.admin_users 的全部 Action，按可审查的顺序排列。
const ACTIONS: &[Register] = action_registry![
    list_users,
    admin_issue_password_reset,
    admin_disable_user,
    admin_enable_user,
];

/// 按注册表顺序挂载全部 Action。
pub(super) fn register_all(module: ModuleSpec, account: Arc<Account>) -> ModuleSpec {
    ACTIONS.iter().fold(module, |module, register| {
        register(module, Arc::clone(&account))
    })
}
