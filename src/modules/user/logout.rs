use yang_base::action::auth::LogoutAction;
use yang_base::router::{ModuleRouter, RouteDescriptor};
use yang_base::BaseError;

pub(super) fn register(router: ModuleRouter) -> Result<ModuleRouter, BaseError> {
    let route = RouteDescriptor::new("POST", "/api/v1/users/logout", "users.logout")?
        .with_success_status(200)?
        .with_tags(vec!["users".to_string()])?;
    router
        .register_action(LogoutAction::new())?
        .register_route("logout", route)
}
