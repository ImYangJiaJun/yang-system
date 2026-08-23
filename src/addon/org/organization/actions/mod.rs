//! 企业 Action 路由表。

mod list;
mod select;

use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let module = module
        .action_fn(action_name("list")?, list::handle)
        .route(HttpMethod::Get, "/api/v1/orgs")
        .display_name("企业列表")
        .description("使用标准 Tables 分页、搜索和排序链查询企业")
        .permissions(["org.org:read"])
        .register();
    let module = module
        .action_fn(action_name("select")?, select::handle)
        .route(HttpMethod::Post, "/api/v1/orgs/options")
        .display_name("企业选择器")
        .description("返回关系字段使用的企业选择项")
        .permissions(["org.org:read"])
        .register();
    // scaffold:action-registration
    Ok(module)
}
