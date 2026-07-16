use super::HttpState;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde_json::json;
use yang_base::action::ApiResponse;

pub(super) fn register(router: Router<HttpState>) -> Router<HttpState> {
    router.route("/health/live", axum::routing::get(handle))
}

async fn handle() -> impl IntoResponse {
    Json(ApiResponse::success_value(
        json!({"status": "live"}),
        "服务存活",
    ))
}
