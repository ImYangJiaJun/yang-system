//! `account.user` Module 的定义与组装入口。

mod actions;
mod browser_session;
mod claims;
mod password;
mod policy;
mod rate_limit;
mod repository;
mod schema;
mod service;

use crate::authorization::AuthorizationVersionValidator;
use crate::config::SecuritySettings;
use crate::modules::account::GrantResolver;
use crate::security::TrustedClientIpMiddleware;
use password::PasswordEngine;
use rate_limit::AuthRateLimiter;
use repository::UserRepository;
use service::UserService;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, UiCatalogAction};
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModuleName, ModulePresentationSpec,
    ModuleSpec,
};
use yang_base::BaseError;

pub(crate) use claims::user_from_claims;

/// 构建用户 Module。
///
/// 此处只聚合 Schema、共享服务、中间件和 Action 清单；具体业务实现留在对应文件中。
pub(super) fn build_module(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let table = schema::user_table_spec()?;
    let users = Arc::new(UserRepository::new(table.table_definition()?));
    let passwords = Arc::new(PasswordEngine::new(security.argon2_max_concurrency)?);
    let rate_limiter = Arc::new(AuthRateLimiter::new(&security));
    let trusted_client_ip = TrustedClientIpMiddleware::from_cidrs(&security.trusted_proxy_cidrs)?;
    let service = Arc::new(UserService::new(
        Arc::clone(&users),
        passwords,
        rate_limiter,
        grant_resolver,
        security.issue_refresh_credential_version,
    ));
    let module = ModuleSpec::new(
        ModuleName::new("account.user")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(table)
    .middleware(trusted_client_ip)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    )
    .native_action(UiCatalogAction);

    let credential_mutations_enabled = security.issue_refresh_credential_version;
    actions::register_all(module, service, credential_mutations_enabled).map(|module| {
        let mut presentation = ModulePresentationSpec::new(
            crate::modules::presentation::user_identity(),
            "用户中心",
            "account",
        )
        .description("查看当前登录账号与管理会话")
        .order(10)
        .primary_action(yang_base::action!("account.user.me"))
        .present_action(
            yang_base::action!("account.user.logout"),
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Invoke),
        );
        if credential_mutations_enabled {
            presentation = presentation.present_action(
                yang_base::action!("account.user.change_password"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            );
        }
        module.presentation(presentation)
    })
}
