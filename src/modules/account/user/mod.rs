//! `account.user` Module 的定义与组装入口。

mod actions;
mod claims;
mod password;
mod policy;
mod rate_limit;
mod repository;
mod schema;
mod service;

use crate::config::SecuritySettings;
use password::PasswordEngine;
use rate_limit::AuthRateLimiter;
use repository::UserRepository;
use service::UserService;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, UiCatalogAction};
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

pub(crate) use claims::user_from_claims;

/// 构建用户 Module。
///
/// 此处只聚合 Schema、共享服务、中间件和 Action 清单；具体业务实现留在对应文件中。
pub(super) fn build_module(security: Arc<SecuritySettings>) -> Result<ModuleSpec, BaseError> {
    let table = schema::user_table_spec()?;
    let users = Arc::new(UserRepository::new(table.table_definition()?));
    let passwords = Arc::new(PasswordEngine::new(security.argon2_max_concurrency)?);
    let rate_limiter = Arc::new(AuthRateLimiter::new(&security));
    let service = Arc::new(UserService::new(users, passwords, rate_limiter));
    let module = ModuleSpec::new(
        ModuleName::new("account.user")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(table)
    .middleware(TokenAuthMiddleware::new(user_from_claims).authenticate_public_actions())
    .native_action(UiCatalogAction);

    actions::register_all(module, service)
}
