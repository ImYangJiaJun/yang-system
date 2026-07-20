//! 平台账号 Addon 的公开组装入口。

mod user;

use crate::modules::account;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

/// 构建平台账号 Addon。
pub fn build_addon() -> Result<AddonSpec, BaseError> {
    let users =
        user::build_module().middleware(TokenAuthMiddleware::new(account::user_from_claims));

    Ok(AddonSpec::new(yang_base::addon!("admin"))
        .depends_on(yang_base::addon!("account"))
        .module(users))
}
