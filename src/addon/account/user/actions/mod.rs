//! 用户 Action 清单：集中路由表。
//!
//! 所有 Action 只在这里进入 `ModuleSpec`，新增接口时无需在多层字符串路由表中重复登记。

mod change_password;
mod disable_self;
mod login;
mod logout;
mod me;
mod refresh;
mod register;
mod request_registration_email;
mod reset_password;
mod step_up;

use super::domain::service::UserService;
use std::sync::Arc;
use yang_base::action::{ActionContext, StepUpManager};
use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

/// 按清晰、可审查的顺序注册用户领域全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    service: Arc<UserService>,
    credential_mutations_enabled: bool,
    step_up_manager: Option<Arc<StepUpManager>>,
) -> Result<ModuleSpec, BaseError> {
    let module = module
        .action_fn(action_name("request_registration_email")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext,
                  input: request_registration_email::RequestRegistrationEmailInput| {
                request_registration_email::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(
            HttpMethod::Post,
            "/api/v1/users/registration-email-verifications",
        )
        .display_name("发送注册邮箱验证码")
        .description("按邮箱、来源地址和全局容量限制投递一次性注册验证码")
        .success_status(202)
        .public()
        .register();
    let module = module
        .action_fn(action_name("register")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext, input: register::RegisterInput| {
                register::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Post, "/api/v1/users/register")
        .display_name("注册用户")
        .description("创建一个新用户")
        .success_status(201)
        .public()
        .register();
    let module = module
        .action_fn(action_name("login")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext, input: login::BrowserLoginInput| {
                login::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Post, "/api/v1/users/login")
        .display_name("登录")
        .description("校验账号密码并签发 Token")
        .public()
        .register();
    let module = module
        .action_fn(action_name("refresh")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext, input: refresh::BrowserRefreshInput| {
                refresh::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Post, "/api/v1/users/refresh")
        .display_name("刷新 Token")
        .description("轮换 Refresh Token 并签发新 Token 对")
        .public()
        .register();
    let module = if credential_mutations_enabled {
        let module = module
            .action_fn(action_name("change_password")?, {
                let service = Arc::clone(&service);
                move |ctx: ActionContext, input: change_password::ChangePasswordInput| {
                    change_password::handle(ctx, input, Arc::clone(&service))
                }
            })
            .route(HttpMethod::Post, "/api/v1/users/change-password")
            .display_name("修改密码")
            .description("校验当前密码并使已有会话失效")
            .register();
        let module = module
            .action_fn(action_name("disable_self")?, {
                let service = Arc::clone(&service);
                move |ctx: ActionContext, input: disable_self::DisableSelfInput| {
                    disable_self::handle(ctx, input, Arc::clone(&service))
                }
            })
            .route(HttpMethod::Post, "/api/v1/users/disable")
            .display_name("停用当前账号")
            .description("停用当前账号及全部平台/企业关系，并撤销此前签发的全部会话")
            .register();
        module
            .action_fn(action_name("reset_password")?, {
                let service = Arc::clone(&service);
                move |ctx: ActionContext, input: reset_password::ResetPasswordInput| {
                    reset_password::handle(ctx, input, Arc::clone(&service))
                }
            })
            .route(HttpMethod::Post, "/api/v1/users/reset-password")
            .display_name("重置密码")
            .description("消费短期单次凭证并使已有会话失效")
            .public()
            .register()
    } else {
        module
    };
    let module = module
        .action_fn(action_name("logout")?, {
            let service = Arc::clone(&service);
            move |ctx: ActionContext, input: logout::BrowserLogoutInput| {
                logout::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Post, "/api/v1/users/logout")
        .display_name("退出全部会话")
        .description("递增持久会话版本并撤销当前账号此前签发的全部 Access 与 Refresh Token")
        .register();
    let module = match step_up_manager {
        Some(manager) => module
            .action_fn(action_name("step_up_complete")?, {
                let service = Arc::clone(&service);
                move |ctx: ActionContext, input: step_up::CompleteStepUpInput| {
                    step_up::handle(ctx, input, Arc::clone(&service), Arc::clone(&manager))
                }
            })
            .route(HttpMethod::Post, "/api/v1/users/step-up/complete")
            .display_name("完成敏感操作重认证")
            .description("重新校验账号密码并把短期 challenge 升级为一次性 proof")
            .public()
            .register(),
        None => module,
    };
    let module = module
        .action_fn(action_name("me")?, {
            move |ctx: ActionContext, input: me::EmptyInput| {
                me::handle(ctx, input, Arc::clone(&service))
            }
        })
        .route(HttpMethod::Get, "/api/v1/users/me")
        .display_name("当前用户")
        .description("读取当前已认证用户")
        .register();
    // scaffold:action-registration
    Ok(module)
}
