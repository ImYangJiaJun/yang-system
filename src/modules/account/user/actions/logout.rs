use yang_base::action::auth::LogoutAction;
use yang_base::definition::{ActionName, ActionSpec, HttpMethod, ModuleSpec, RouteSpec};
use yang_base::BaseError;

pub(super) fn register(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let name =
        ActionName::new("logout").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let spec = ActionSpec::new(
        name,
        RouteSpec::new(HttpMethod::Post, "/api/v1/users/logout", "users.logout"),
    )
    .display_name("退出登录")
    .description("撤销当前 Token")
    .public(true)
    .tag("users");
    Ok(module.action(spec, LogoutAction::new()))
}
