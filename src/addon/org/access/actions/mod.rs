//! `org.tenant` Action 路由表。

mod create;
mod list;

use super::domain::service::TenantService;
use create::TenantCreateInput;
use list::TenantListInput;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<TenantService>,
) -> Result<ModuleSpec, BaseError> {
    let module = module
        .action_fn(action_name("create")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext, input: TenantCreateInput| {
                create::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Post, "/api/v1/tenants")
        .display_name("创建企业")
        .description("原子创建企业与当前用户的初始成员关系")
        .success_status(201)
        .register();
    let module = module
        .action_fn(action_name("list")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext, input: TenantListInput| {
                list::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Get, "/api/v1/tenants")
        .display_name("我的企业")
        .description("在选择租户前返回当前用户可访问的企业")
        .register();
    // scaffold:action-registration
    Ok(module)
}
