//! `account.user` Module 的定义与组装入口。

mod actions;
mod claims;
mod schema;
mod service;

use crate::config::SecuritySettings;
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
    let service = Arc::new(UserService::new(security));
    let module = ModuleSpec::new(
        ModuleName::new("account.user")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(schema::user_table_spec()?)
    .middleware(TokenAuthMiddleware::new(user_from_claims).authenticate_public_actions())
    .native_action(UiCatalogAction);

    actions::register_all(module, service)
}
