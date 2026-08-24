//! 账号 Addon 的公开组装入口。

mod domain;
mod user;

use crate::authorization::AuthorizationVersionValidator;
use crate::authorization::StepUpServices;
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

#[cfg(feature = "admin")]
pub(crate) use domain::authz_version::LockedUserAuthorization;
pub(crate) use domain::authz_version::{
    disable_locked_user_and_increment_versions, find_authorization_version,
    increment_locked_credential_versions, lock_user_credential, LockedUserCredential,
};
#[cfg(any(feature = "admin", feature = "org"))]
pub(crate) use domain::authz_version::{increment_locked_authz_version, lock_user_authorization};
#[cfg(feature = "org")]
pub(crate) use domain::authz_version::{increment_locked_authz_versions, lock_user_authorizations};
pub use domain::email_delivery;
pub(crate) use domain::grants::{AuthorizationGrants, CompositeGrantResolver, GrantResolver};
pub(crate) use domain::password_reset::{
    consume_in_tx as consume_password_reset_in_tx,
    find_target_user as find_password_reset_target_user, invalid_reset_token,
    lock_in_tx as lock_password_reset_in_tx, PasswordResetReference,
};
#[cfg(feature = "admin")]
pub(crate) use domain::password_reset::{
    create_in_tx as create_password_reset_in_tx, GeneratedPasswordReset,
};
pub(crate) use domain::system_owner::{OwnerClaimOutcome, SystemOwnerClaimer};
#[cfg(any(
    feature = "admin",
    feature = "observability",
    feature = "org",
    feature = "work"
))]
pub(crate) use user::user_from_claims;
pub(crate) use user::UserStatus;
#[cfg(feature = "admin")]
pub(crate) use user::{AuthOperation, AuthRateLimiter};

/// 返回不声明最终管理员的默认声明器（`admin` Addon 未启用时由组合根注入）。
#[cfg(not(feature = "admin"))]
pub(crate) fn no_system_owner_claimer() -> Arc<dyn SystemOwnerClaimer> {
    Arc::new(domain::system_owner::NoSystemOwnerClaimer)
}

/// 构建账号 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `account.user`。
pub(crate) fn build_addon(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    system_owner_claimer: Arc<dyn SystemOwnerClaimer>,
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
) -> Result<AddonSpec, BaseError> {
    Ok(
        AddonSpec::new(yang_base::addon!("account")).module(user::build_module(
            security,
            grant_resolver,
            system_owner_claimer,
            authorization_validator,
            step_up,
        )?),
    )
}
