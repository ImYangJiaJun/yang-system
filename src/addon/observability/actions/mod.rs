//! 前端诊断 Action 路由表。

mod report;

use super::domain::FrontendErrorRateLimiter;
use std::sync::Arc;
use yang_base::definition::{ActionName, HttpMethod, ModuleSpec};
use yang_base::BaseError;

fn action_name(value: &str) -> Result<ActionName, BaseError> {
    ActionName::new(value).map_err(|error| BaseError::ConfigError(error.to_string()))
}

pub(super) fn register_all(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let rate_limiter = Arc::new(FrontendErrorRateLimiter::default());
    let module = module
        .action_fn(action_name("report_frontend_error")?, {
            move |ctx, input| report::handle(ctx, input, Arc::clone(&rate_limiter))
        })
        .route(HttpMethod::Post, "/api/v1/observability/frontend-errors")
        .display_name("上报前端错误")
        .description("记录已认证浏览器的无敏感正文错误指纹与后端请求关联")
        .register();
    // scaffold:action-registration
    Ok(module)
}
