//! `account.user` Module（module 层）：用户模块装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表、
//! Step-up 守卫与展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::avatar::AvatarRepository;
use super::domain::context::Account;
use super::domain::login_event::LoginEventRepository;
use super::domain::repository::UserRepository;
use super::domain::session::SessionRepository;
use super::{GrantResolver, SystemOwnerClaimer};
use crate::authorization::StepUpServices;
use crate::authorization::{AuthorizationVersionValidator, RequestFingerprintResolver};
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::action::{TokenAuthMiddleware, UiCatalogAction};
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModuleName, ModulePresentationSpec,
    ModuleSpec,
};
use yang_base::transport::client_ip::TrustedClientIpMiddleware;
use yang_base::BaseError;

/// 装配 `account.user` Module：表 → 上下文 → 中间件 → Action 注册表 → Step-up → 展示投影。
pub(super) fn build_module(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
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
        step_up.as_ref().map(StepUpServices::manager),
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
    if let Some(step_up) = step_up {
        for target in step_up_targets(
            account.credential_mutations_enabled(),
            account.totp_settings().is_some(),
        ) {
            module = module.middleware(step_up.middleware(
                target,
                RequestFingerprintResolver::global("account-session"),
            ));
        }
    }
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

/// 需要 Step-up 重认证的账号安全 Action。
///
/// `totp_enabled` 对应 `security.totp` 配置段：TOTP Action 未注册时
/// 不能为其挂 Step-up 中间件（构建期会校验 ActionRef 有效性）。
fn step_up_targets(
    credential_mutations_enabled: bool,
    totp_enabled: bool,
) -> Vec<yang_base::definition::ActionRef> {
    // 管理写操作（D-1/D-2）与自助安全操作要求 Step-up；
    // 退出登录（logout）是收敛性操作，不制造新的风险面，无需重认证。
    let mut targets = vec![
        yang_base::action!("account.user.admin_disable_user"),
        yang_base::action!("account.user.admin_enable_user"),
        yang_base::action!("account.user.admin_issue_password_reset"),
        // 逐台撤销是安全操作（踢出某设备），且 jti 黑名单不依赖凭据版本签发，
        // 无论凭据变更开关是否打开都必须重认证。
        yang_base::action!("account.user.revoke_session"),
    ];
    // TOTP 停用是安全降级操作，不受凭据写开关影响；setup/activate 是认证器
    // 生命周期变更（会话劫持者可借 setup→activate 把 TOTP 绑到自己并夺走恢复码，
    // 进而锁定合法用户），三者都必须重认证；已激活账号的 Step-up 会同时要求
    // 出示第二因子。
    if totp_enabled {
        targets.push(yang_base::action!("account.user.totp_setup"));
        targets.push(yang_base::action!("account.user.totp_activate"));
        targets.push(yang_base::action!("account.user.totp_deactivate"));
    }
    if credential_mutations_enabled {
        targets.insert(0, yang_base::action!("account.user.delete_account"));
        targets.insert(1, yang_base::action!("account.user.change_email"));
        targets.insert(2, yang_base::action!("account.user.change_username"));
        targets.insert(3, yang_base::action!("account.user.disable_self"));
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_account_security_mutation_is_explicitly_step_up_protected() {
        assert_eq!(
            step_up_targets(true, true),
            vec![
                yang_base::action!("account.user.delete_account"),
                yang_base::action!("account.user.change_email"),
                yang_base::action!("account.user.change_username"),
                yang_base::action!("account.user.disable_self"),
                yang_base::action!("account.user.admin_disable_user"),
                yang_base::action!("account.user.admin_enable_user"),
                yang_base::action!("account.user.admin_issue_password_reset"),
                yang_base::action!("account.user.revoke_session"),
                yang_base::action!("account.user.totp_setup"),
                yang_base::action!("account.user.totp_activate"),
                yang_base::action!("account.user.totp_deactivate"),
            ]
        );
        assert_eq!(
            step_up_targets(false, true),
            vec![
                yang_base::action!("account.user.admin_disable_user"),
                yang_base::action!("account.user.admin_enable_user"),
                yang_base::action!("account.user.admin_issue_password_reset"),
                yang_base::action!("account.user.revoke_session"),
                yang_base::action!("account.user.totp_setup"),
                yang_base::action!("account.user.totp_activate"),
                yang_base::action!("account.user.totp_deactivate"),
            ]
        );
        // TOTP 配置段缺失时 deactivate 不注册，step-up 清单不得引用它。
        assert_eq!(
            step_up_targets(false, false),
            vec![
                yang_base::action!("account.user.admin_disable_user"),
                yang_base::action!("account.user.admin_enable_user"),
                yang_base::action!("account.user.admin_issue_password_reset"),
                yang_base::action!("account.user.revoke_session"),
            ]
        );
    }
}
