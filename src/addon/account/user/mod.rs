//! `account.user` Module（module 层）：用户模块装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表与
//! 展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::avatar::AvatarRepository;
use super::domain::context::Account;
use super::domain::login_event::LoginEventRepository;
use super::domain::repository::UserRepository;
use super::domain::session::SessionRepository;
use super::{GrantResolver, SystemAuthorizationPort, SystemOwnerClaimer};
use crate::authorization::AuthorizationVersionValidator;
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, UiCatalogAction};
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModuleName, ModulePresentationSpec,
    ModuleSpec,
};
use yang_base::transport::client_ip::TrustedClientIpMiddleware;
use yang_base::BaseError;

/// 装配 `account.user` Module：表 → 上下文 → 中间件 → Action 注册表 → 展示投影。
pub(super) fn build_module(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    system_authorization: Arc<dyn SystemAuthorizationPort>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let table = table::user_table_spec()?;
    let session_repository = SessionRepository::new(crate::schema::user_session()?);
    let login_event_repository = LoginEventRepository::new(crate::schema::login_event()?);
    let account = Arc::new(Account::new(
        UserRepository::new(table.table_definition()?),
        session_repository,
        login_event_repository,
        AvatarRepository::new(crate::schema::user_avatar()?),
        &security,
        grant_resolver,
        system_owner_claimer,
        system_authorization,
    )?);

    let mut module = ModuleSpec::new(
        ModuleName::new("account.user")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(table)
    .middleware(TrustedClientIpMiddleware::from_cidrs(
        &security.trusted_proxy_cidrs,
    )?)
    .middleware(
        TokenAuthMiddleware::new(super::domain::claims::user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    )
    .native_action(UiCatalogAction);
    module = actions::register_all(module, Arc::clone(&account));
    Ok(module.presentation(presentation(account.credential_mutations_enabled())))
}

/// 前端展示投影（用户中心导航）。
fn presentation(credential_mutations_enabled: bool) -> ModulePresentationSpec {
    let mut presentation =
        ModulePresentationSpec::new(crate::addon::user_identity(), "用户中心", "account")
            .description("查看当前登录账号与管理会话")
            .order(10)
            .primary_action(yang_base::action!("account.user.me"))
            .present_action(
                yang_base::action!("account.user.logout"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Invoke),
            );
    if credential_mutations_enabled {
        presentation = presentation
            .present_action(
                yang_base::action!("account.user.change_password"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            )
            .present_action(
                yang_base::action!("account.user.change_username"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            )
            .present_action(
                yang_base::action!("account.user.change_email"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            )
            .present_action(
                yang_base::action!("account.user.disable_self"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Invoke),
            );
    }
    presentation
}
