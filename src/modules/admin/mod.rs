//! 平台账号 Addon 的公开组装入口。

mod grants;
mod user;

use crate::authorization::AuthorizationVersionValidator;
use crate::config::SecuritySettings;
use crate::modules::account;
use crate::modules::account::GrantResolver;
use crate::security::{RequestFingerprintResolver, StepUpServices};
use grants::AdminGrantResolver;
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

/// 返回平台账号域的 Token 授权解析器。
pub(crate) fn grant_resolver() -> Arc<dyn GrantResolver> {
    Arc::new(AdminGrantResolver)
}

/// 构建平台账号 Addon。
pub fn build_addon(
    security: Arc<SecuritySettings>,
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
) -> Result<AddonSpec, BaseError> {
    let mut users = user::build_module(&security)?.middleware(
        TokenAuthMiddleware::new(account::user_from_claims)
            .with_claims_validator(authorization_validator),
    );
    if let Some(step_up) = step_up {
        for target in user::step_up_targets() {
            users = users.middleware(
                step_up.middleware(target, RequestFingerprintResolver::global("admin-user")),
            );
        }
    }

    Ok(AddonSpec::new(yang_base::addon!("admin"))
        .depends_on(yang_base::addon!("account"))
        .module(users))
}
