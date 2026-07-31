//! `system.observability` Module 组装。

mod actions;

use crate::authorization::AuthorizationVersionValidator;
use crate::modules::account::user_from_claims;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let module = ModuleSpec::new(
        ModuleName::new("system.observability")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .middleware(
        TokenAuthMiddleware::new(user_from_claims).with_claims_validator(authorization_validator),
    );
    Ok(actions::register_all(module))
}
