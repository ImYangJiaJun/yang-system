//! 账号 Addon 的公开组装入口。

mod authz_version;
mod grants;
mod password_reset;
mod user;

use crate::authorization::AuthorizationVersionValidator;
use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

pub(crate) use authz_version::{
    find_authorization_version, increment_locked_authz_version, increment_locked_authz_versions,
    increment_locked_credential_versions, lock_user_authorization, lock_user_authorizations,
    lock_user_credential, LockedUserAuthorization, LockedUserCredential,
};
pub(crate) use grants::{AuthorizationGrants, CompositeGrantResolver, GrantResolver};
pub(crate) use password_reset::{
    consume_in_tx as consume_password_reset_in_tx, create_in_tx as create_password_reset_in_tx,
    find_target_user as find_password_reset_target_user, invalid_reset_token,
    lock_in_tx as lock_password_reset_in_tx, GeneratedPasswordReset, PasswordResetReference,
};
pub(crate) use user::user_from_claims;
pub(crate) use user::{AuthOperation, AuthRateLimiter};

/// 构建账号 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `account.user`。
pub fn build_addon(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    Ok(
        AddonSpec::new(yang_base::addon!("account")).module(user::build_module(
            security,
            grant_resolver,
            authorization_validator,
        )?),
    )
}
