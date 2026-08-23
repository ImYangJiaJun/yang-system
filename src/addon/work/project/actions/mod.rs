//! 项目 Module 的显式关系选择 Action 路由表。

mod options;

use std::sync::Arc;
use yang_base::action::builtin::RelationOptionsAction;
use yang_base::action::ActionContext;
use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::table::RelationOptionsRequest;
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let inner = Arc::new(RelationOptionsAction::new("id", ["name"])?);
    let module = module
        .action_fn(action_name("options")?, {
            let inner = Arc::clone(&inner);
            move |ctx: ActionContext, input: RelationOptionsRequest| {
                options::handle(ctx, input, Arc::clone(&inner))
            }
        })
        .route(HttpMethod::Post, "/api/v1/work/projects/options")
        .display_name("项目选择器")
        .description("按当前个人工作区分页搜索项目")
        .permissions(["work.project:read"])
        .register();
    // scaffold:action-registration
    Ok(module)
}
