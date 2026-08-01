//! 浏览器到后端的诊断关联边界。

mod actions;

use crate::authorization::AuthorizationVersionValidator;
use crate::modules::account::user_from_claims;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{AddonSpec, ModuleName, ModuleSpec};
use yang_base::BaseError;

pub fn build_addon(
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    let module = ModuleSpec::new(
        ModuleName::new("system.observability")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .middleware(
        TokenAuthMiddleware::new(user_from_claims).with_claims_validator(authorization_validator),
    );
    Ok(AddonSpec::new(yang_base::addon!("system")).module(actions::register_all(module)))
}
