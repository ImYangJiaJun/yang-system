//! 验收 Action 注册表；每个 Action 的定义与实现独占一个文件。
//!
//! 每个 Action 的输入、路由与验收用例都自包含在同名文件中；
//! 这里只有模块清单和注册表数组，新增接口时加 `mod` 声明和数组一行即可。

mod add;
mod bulk_delete;
mod category_options;
mod delete;
mod download;
mod echo;
mod edit;
mod insight;
mod list;
mod preview;
mod redirect;
mod upload;

use super::model::DemoItems;
use std::path::PathBuf;
use yang_base::action::UiCatalogAction;
use yang_base::definition::ModuleSpec;

/// 无外部依赖的注册函数签名。
type SimpleRegister = fn(ModuleSpec) -> ModuleSpec;
/// 依赖验收 fixture 文件的注册函数签名。
type FixtureRegister = fn(ModuleSpec, PathBuf) -> ModuleSpec;
/// 依赖演示内存数据的注册函数签名。
type ItemsRegister = fn(ModuleSpec, DemoItems) -> ModuleSpec;

/// 把模块名展开为它的自包含注册函数；数组每一行就是一个接口。
macro_rules! action_registry {
    ($register:ty; $($action:ident),* $(,)?) => {
        &[$($action::register as $register),*]
    };
}

/// demo.api 的无依赖 Action。
const API_ACTIONS: &[SimpleRegister] = action_registry![SimpleRegister;
    echo,
    upload,
    redirect,
];

/// demo.api 依赖验收 fixture 文件的 Action。
const FIXTURE_ACTIONS: &[FixtureRegister] = action_registry![FixtureRegister;
    download,
    preview,
];

/// demo.category 的 Action。
const CATEGORY_ACTIONS: &[SimpleRegister] = action_registry![SimpleRegister;
    category_options,
];

/// demo.items 的 Action。
const ITEMS_ACTIONS: &[ItemsRegister] = action_registry![ItemsRegister;
    list,
    add,
    edit,
    delete,
    bulk_delete,
    insight,
    // scaffold:action-registration
];

/// 注册 demo.api 模块的全部验收 Action。
pub(super) fn register_api(module: ModuleSpec, fixture: PathBuf) -> ModuleSpec {
    let module = module.native_action(UiCatalogAction);
    let module = API_ACTIONS
        .iter()
        .fold(module, |module, register| register(module));
    FIXTURE_ACTIONS
        .iter()
        .fold(module, |module, register| register(module, fixture.clone()))
}

/// 注册 demo.category 模块的全部验收 Action。
pub(super) fn register_category(module: ModuleSpec) -> ModuleSpec {
    CATEGORY_ACTIONS
        .iter()
        .fold(module, |module, register| register(module))
}

/// 注册 demo.items 模块的全部验收 Action。
pub(super) fn register_items(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    ITEMS_ACTIONS.iter().fold(module, |module, register| {
        register(module, DemoItems::clone(&items))
    })
}
