//! 便签 Action 注册表。
//!
//! 每个 Action 的输入、路由、权限与业务用例都自包含在同名文件中；
//! 这里只有模块清单和注册表数组，新增接口时加 `mod` 声明和数组一行即可。

mod create_note;
mod delete_note;
mod list_notes;
mod update_note;

use crate::addon::demo::domain::context::Demo;
use std::sync::Arc;
use yang_base::definition::ModuleSpec;

/// 注册函数的统一签名。
type Register = fn(ModuleSpec, Arc<Demo>) -> ModuleSpec;

/// 把模块名展开为它的自包含注册函数；数组每一行就是一个接口。
macro_rules! action_registry {
    ($($action:ident),* $(,)?) => {
        &[$($action::register as Register),*]
    };
}

/// demo.notes 的全部 Action，按可审查的顺序排列。
const ACTIONS: &[Register] = action_registry![
    create_note,
    update_note,
    delete_note,
    list_notes,
    // scaffold:action-registration
];

/// 按注册表顺序挂载全部 Action。
pub(super) fn register_all(module: ModuleSpec, demo: Arc<Demo>) -> ModuleSpec {
    ACTIONS.iter().fold(module, |module, register| {
        register(module, Arc::clone(&demo))
    })
}
