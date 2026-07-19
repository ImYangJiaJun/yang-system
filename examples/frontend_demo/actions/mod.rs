//! 验收 Action 聚合；每个 Action 的定义与实现独占一个文件。

mod add;
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
use std::sync::Arc;
use yang_base::action::UiCatalogAction;
use yang_base::definition::ModuleSpec;

pub(super) fn register_api(module: ModuleSpec, fixture: PathBuf) -> ModuleSpec {
    let module = module.native_action(UiCatalogAction);
    let module = echo::register(module);
    let module = upload::register(module);
    let module = download::register(module, fixture.clone());
    let module = preview::register(module, fixture);
    redirect::register(module)
}

pub(super) fn register_category(module: ModuleSpec) -> ModuleSpec {
    category_options::register(module)
}

pub(super) fn register_items(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    let module = list::register(module, Arc::clone(&items));
    let module = add::register(module, Arc::clone(&items));
    let module = edit::register(module, Arc::clone(&items));
    let module = delete::register(module, Arc::clone(&items));
    insight::register(module, items)
}
