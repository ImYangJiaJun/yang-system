//! 企业成员显式写 Action 路由表。
//!
//! 三个写 Action 的 Catalog 契约（动态表驱动 Schema、`org.user:write` 权限、
//! add 的 201）由 `crud_at_with_mutations` 统一生成；函数式 Handler 经
//! `domain::FnAction` 桥接为 `DynAction`。

mod add;
mod delete;
mod put;

use super::domain::FnAction;
use yang_base::definition::ModuleSpec;
use yang_base::BaseError;

pub(super) fn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    module.crud_at_with_mutations(
        "/api/v1/org/users",
        FnAction::new("add", add::handle),
        FnAction::new("put", put::handle),
        FnAction::new("del", delete::handle),
    )
}
