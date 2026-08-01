//! `account.user` Module 的定义与组装入口。

mod actions;
mod browser_session;
mod claims;
mod email_verification;
mod lifecycle;
mod password;
mod policy;
mod rate_limit;
mod repository;
mod schema;
mod service;
mod status;

use crate::authorization::AuthorizationVersionValidator;
use crate::authorization::{RequestFingerprintResolver, StepUpServices};
use crate::config::SecuritySettings;
use crate::modules::account::{GrantResolver, SystemOwnerClaimer};
use password::PasswordEngine;
use repository::UserRepository;
use service::UserService;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, UiCatalogAction};
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModuleName, ModulePresentationSpec,
    ModuleSpec,
};
use yang_base::transport::client_ip::TrustedClientIpMiddleware;
use yang_base::BaseError;

pub(crate) use claims::user_from_claims;
pub(crate) use rate_limit::{AuthOperation, AuthRateLimiter};
pub(crate) use status::UserStatus;

/// 构建用户 Module。
///
/// 此处只聚合 Schema、共享服务、中间件和 Action 清单；具体业务实现留在对应文件中。
pub(super) fn build_module(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
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
        system_owner_claimer,
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
    let step_up_manager = step_up.as_ref().map(StepUpServices::manager);
    let module = actions::register_all(
        module,
        service,
        credential_mutations_enabled,
        step_up_manager,
    )?;
    let mut module = module;
    if let Some(step_up) = step_up {
        for target in step_up_targets(credential_mutations_enabled) {
            module = module.middleware(step_up.middleware(
                target,
                RequestFingerprintResolver::global("account-session"),
            ));
        }
    }
    Ok({
        let mut presentation =
            ModulePresentationSpec::new(crate::modules::user_identity(), "用户中心", "account")
                .description("查看当前登录账号与管理会话")
                .order(10)
                .primary_action(yang_base::action!("account.user.me"))
                .present_action(
                    yang_base::action!("account.user.logout"),
                    ActionPresentationSpec::new(
                        ActionPlacement::Toolbar,
                        ActionInteraction::Invoke,
                    ),
                );
        if credential_mutations_enabled {
            presentation = presentation
                .present_action(
                    yang_base::action!("account.user.change_password"),
                    ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
                )
                .present_action(
                    yang_base::action!("account.user.disable_self"),
                    ActionPresentationSpec::new(
                        ActionPlacement::Toolbar,
                        ActionInteraction::Invoke,
                    ),
                );
        }
        module.presentation(presentation)
    })
}

pub(crate) fn step_up_targets(
    credential_mutations_enabled: bool,
) -> Vec<yang_base::definition::ActionRef> {
    let mut targets = vec![yang_base::action!("account.user.logout")];
    if credential_mutations_enabled {
        targets.insert(0, yang_base::action!("account.user.disable_self"));
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_account_security_mutation_is_explicitly_step_up_protected() {
        assert_eq!(
            step_up_targets(true),
            vec![
                yang_base::action!("account.user.disable_self"),
                yang_base::action!("account.user.logout"),
            ]
        );
        assert_eq!(
            step_up_targets(false),
            vec![yang_base::action!("account.user.logout")]
        );
    }
}
