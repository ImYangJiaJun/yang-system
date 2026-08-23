//! 任务 Module 的显式业务 Action 路由表。
//!
//! options/complete 经 `action_fn` 原子注册；add/put 的 Catalog 契约（动态表驱动
//! Schema、`work.task:write` 权限、add 的 201）由 `crud_at_with_mutations`
//! 统一生成，函数式 Handler 经 `domain::FnAction` 桥接为 `DynAction`。

mod add;
mod complete;
mod options;
mod put;

use super::domain::FnAction;
use std::sync::Arc;
use yang_base::action::builtin::{DelAction, RelationOptionsAction};
use yang_base::action::ActionContext;
use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::table::RelationOptionsRequest;
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let inner = Arc::new(RelationOptionsAction::new("id", ["title"])?);
    let module = module
        .action_fn(action_name("options")?, {
            let inner = Arc::clone(&inner);
            move |ctx: ActionContext, input: RelationOptionsRequest| {
                options::handle(ctx, input, Arc::clone(&inner))
            }
        })
        .route(HttpMethod::Post, "/api/v1/work/tasks/options")
        .display_name("父任务选择器")
        .description("按当前个人工作区分页搜索父任务")
        .permissions(["work.task:read"])
        .register();
    let module = module
        .action_fn(action_name("complete")?, complete::handle)
        .route(HttpMethod::Post, "/api/v1/work/tasks/complete")
        .display_name("批量完成")
        .description("一次将最多 100 个当前工作区任务标记为完成")
        .permissions(["work.task:write"])
        .register();
    // scaffold:action-registration
    module.crud_at_with_mutations(
        "/api/v1/work/tasks",
        FnAction::new("add", add::handle),
        FnAction::new("put", put::handle),
        DelAction::new(),
    )
}
