//! 浏览器到后端的诊断关联边界。

mod frontend;

use crate::authorization::AuthorizationVersionValidator;
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

pub fn build_addon(
    authorization_validator: AuthorizationVersionValidator,
) -> Result<AddonSpec, BaseError> {
    Ok(AddonSpec::new(yang_base::addon!("system"))
        .module(frontend::build_module(authorization_validator)?))
}
