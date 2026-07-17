use super::HttpState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use yang_base::action::ApiResponse;

pub(super) fn register(router: Router<HttpState>) -> Router<HttpState> {
    router.route("/health/ready", axum::routing::get(handle))
}

async fn handle(State(state): State<HttpState>) -> Response {
    let health = state.app.tools().health_check().await;
    if health.is_healthy() {
        (
            StatusCode::OK,
            Json(ApiResponse::success_value(
                json!({"status": "ready"}),
                "服务就绪",
            )),
        )
            .into_response()
    } else {
        tracing::warn!(?health, "就绪检查失败");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::fail(900001, "服务尚未就绪")),
        )
            .into_response()
    }
}
