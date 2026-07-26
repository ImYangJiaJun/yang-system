//! 账号 Addon 的公开组装入口。

mod authz_version;
mod grants;
mod user;

use crate::config::SecuritySettings;
use std::sync::Arc;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

pub(crate) use authz_version::{
    increment_locked_authz_version, increment_locked_authz_versions, lock_user_authorization,
    lock_user_authorizations, LockedUserAuthorization,
};
pub(crate) use grants::{AuthorizationGrants, CompositeGrantResolver, GrantResolver};
pub(crate) use user::user_from_claims;

/// 构建账号 Addon。
///
/// Addon 边界负责声明产品能力及其 Module；应用层不应直接拼装 `account.user`。
pub fn build_addon(
    security: Arc<SecuritySettings>,
    grant_resolver: Arc<dyn GrantResolver>,
) -> Result<AddonSpec, BaseError> {
    Ok(AddonSpec::new(yang_base::addon!("account"))
        .module(user::build_module(security, grant_resolver)?))
}
