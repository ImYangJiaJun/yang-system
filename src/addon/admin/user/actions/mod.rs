//! 平台账号 Action 路由表。

mod add;
mod create_password_reset;
mod list;
mod set_admin;
mod set_status;

use super::domain::AdminService;
use std::sync::Arc;
use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<AdminService>,
    password_reset_enabled: bool,
) -> Result<ModuleSpec, BaseError> {
    let module = module
        .action_fn(action_name("list")?, {
            let service = Arc::clone(&service);
            move |ctx, input| list::handle(ctx, input, Arc::clone(&service))
        })
        .route(HttpMethod::Get, "/api/v1/admin/users")
        .display_name("平台账号列表")
        .description("分页查询平台账号及其基础用户身份")
        .permissions(["admin.user:read"])
        .register();
    let module = module
        .action_fn(action_name("add")?, {
            let service = Arc::clone(&service);
            move |ctx, input| add::handle(ctx, input, Arc::clone(&service))
        })
        .route(HttpMethod::Post, "/api/v1/admin/users")
        .display_name("添加平台账号")
        .description("将现有启用用户绑定为平台账号")
        .permissions(["admin.user:write"])
        .success_status(201)
        .register();
    let module = if password_reset_enabled {
        module
            .action_fn(action_name("create_password_reset")?, {
                let service = Arc::clone(&service);
                move |ctx, input| create_password_reset::handle(ctx, input, Arc::clone(&service))
            })
            .route(HttpMethod::Post, "/api/v1/admin/users/password-reset")
            .display_name("创建密码重置凭证")
            .description("为目标用户创建短期单次消费凭证，响应只返回一次原始凭证")
            .permissions(["admin.user:write"])
            .success_status(201)
            .register()
    } else {
        module
    };
    let module = module
        .action_fn(action_name("set_status")?, {
            let service = Arc::clone(&service);
            move |ctx, input| set_status::handle(ctx, input, Arc::clone(&service))
        })
        .route(HttpMethod::Put, "/api/v1/admin/users/status")
        .display_name("设置平台账号状态")
        .description("启用或停用平台账号，并保护最后一个启用中的超级管理员")
        .permissions(["admin.user:write"])
        .register();
    let module = module
        .action_fn(action_name("set_admin")?, {
            move |ctx, input| set_admin::handle(ctx, input, Arc::clone(&service))
        })
        .route(HttpMethod::Put, "/api/v1/admin/users/admin")
        .display_name("设置超级管理员")
        .description("授予或撤销超级管理员身份，并保护最后一个启用中的超级管理员")
        .permissions(["admin.user:write"])
        .register();
    // scaffold:action-registration
    Ok(module)
}
