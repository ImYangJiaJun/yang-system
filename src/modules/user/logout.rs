use yang_base::action::auth::LogoutAction;
use yang_base::router::Api;

pub(super) fn api() -> Api {
    Api::post("/api/v1/users/logout", LogoutAction::new())
        .operation_id("users.logout")
        .tag("users")
}
