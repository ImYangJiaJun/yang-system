use super::HttpState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use yang_base::action::ApiResponse;
use yang_base::database::GlobalRedis;

pub(super) fn register(router: Router<HttpState>) -> Router<HttpState> {
    router.route("/health/ready", axum::routing::get(handle))
}

async fn handle(State(state): State<HttpState>) -> Response {
    let mysql_ready = sqlx::query("SELECT 1")
        .execute(state.pool.as_ref())
        .await
        .is_ok();
    let redis_ready = GlobalRedis::health_check().await.unwrap_or(false);
    if mysql_ready && redis_ready {
        (
            StatusCode::OK,
            Json(ApiResponse::success_value(
                json!({"status": "ready"}),
                "服务就绪",
            )),
        )
            .into_response()
    } else {
        tracing::warn!(mysql_ready, redis_ready, "就绪检查失败");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::fail(900001, "服务尚未就绪")),
        )
            .into_response()
    }
}
