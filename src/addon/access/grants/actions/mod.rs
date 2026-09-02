//! 授权管理 Action 注册表。
//!
//! 每个 Action 的输入、路由、权限与业务用例都自包含在同名文件中；
//! 这里只有模块清单和注册表数组，新增接口时加 `mod` 声明和数组一行即可。

mod grant_permission;
mod list_permissions;
mod list_user_grants;
mod revoke_permission;

use crate::addon::access::domain::context::Access;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;

/// 注册函数的统一签名。
type Register = fn(ModuleSpec, Arc<Access>) -> ModuleSpec;

/// 把模块名展开为它的自包含注册函数；数组每一行就是一个接口。
macro_rules! action_registry {
    ($($action:ident),* $(,)?) => {
        &[$($action::register as Register),*]
    };
}

/// access.grants 的全部 Action，按可审查的顺序排列。
const ACTIONS: &[Register] = action_registry![
    grant_permission,
    revoke_permission,
    list_user_grants,
    list_permissions,
    // scaffold:action-registration
];

/// 按注册表顺序挂载全部 Action。
pub(super) fn register_all(module: ModuleSpec, access: Arc<Access>) -> ModuleSpec {
    ACTIONS.iter().fold(module, |module, register| {
        register(module, Arc::clone(&access))
    })
}
