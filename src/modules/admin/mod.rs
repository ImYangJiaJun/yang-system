//! 平台账号 Addon 的公开组装入口。

mod grants;
mod user;

use crate::modules::account;
use crate::modules::account::GrantResolver;
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
pub fn build_addon() -> Result<AddonSpec, BaseError> {
    let users =
        user::build_module().middleware(TokenAuthMiddleware::new(account::user_from_claims));

    Ok(AddonSpec::new(yang_base::addon!("admin"))
        .depends_on(yang_base::addon!("account"))
        .module(users))
}
