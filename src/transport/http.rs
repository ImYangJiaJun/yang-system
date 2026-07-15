use anyhow::{bail, Context};
use axum::body::to_bytes;
use axum::extract::{ConnectInfo, Path, Request as AxumRequest, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{on, MethodFilter};
use axum::{Json, Router};
use serde_json::json;
use sqlx::MySqlPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use yang_base::action::{ActionContext, ApiResponse, GlobalTools, Request, RequestMeta};
use yang_base::database::GlobalRedis;
use yang_base::router::AppRouter;
use yang_base::{BaseError, ErrorCategory};

#[derive(Clone)]
struct HttpState {
    app_router: Arc<AppRouter>,
    tools: Arc<GlobalTools>,
    pool: Arc<MySqlPool>,
    local_addr: SocketAddr,
    max_body_bytes: usize,
}

pub async fn serve(
    bind: SocketAddr,
    app_router: Arc<AppRouter>,
    tools: Arc<GlobalTools>,
    pool: Arc<MySqlPool>,
    max_body_bytes: usize,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("绑定 HTTP 地址失败: {bind}"))?;
    let local_addr = listener.local_addr().context("读取 HTTP 监听地址失败")?;
    let state = HttpState {
        app_router,
        tools,
        pool,
        local_addr,
        max_body_bytes,
    };
    let router = build_router(state)?;
    tracing::info!(address = %local_addr, "HTTP 服务已启动");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("HTTP 服务运行失败")
}

fn build_router(state: HttpState) -> anyhow::Result<Router> {
    let catalog = state
        .app_router
        .catalog()
        .context("构建 API Catalog 失败")?;
    let mut router = Router::new()
        .route("/health/live", axum::routing::get(live))
        .route("/health/ready", axum::routing::get(ready));

    for module in catalog.modules {
        for action in module.actions {
            let method_filter = method_filter(&action.route.method)?;
            let path = action.route.path.clone();
            let module_name = module.name.clone();
            let action_name = action.name.clone();
            let success_status = action.route.success_status;
            router = router.route(
                &path,
                on(
                    method_filter,
                    move |State(state): State<HttpState>,
                          ConnectInfo(peer): ConnectInfo<SocketAddr>,
                          Path(path_params): Path<HashMap<String, String>>,
                          request: AxumRequest| {
                        let module_name = module_name.clone();
                        let action_name = action_name.clone();
                        async move {
                            dispatch_request(
                                state,
                                peer,
                                path_params,
                                request,
                                &module_name,
                                &action_name,
                                success_status,
                            )
                            .await
                        }
                    },
                ),
            );
        }
    }

    Ok(router.with_state(state).layer(TraceLayer::new_for_http()))
}

async fn dispatch_request(
    state: HttpState,
    peer: SocketAddr,
    path_params: HashMap<String, String>,
    request: AxumRequest,
    module_name: &str,
    action_name: &str,
    success_status: u16,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, state.max_body_bytes).await {
        Ok(body) => body,
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                BaseError::ParamInvalid("body".to_string(), "请求体过大".to_string()),
            )
        }
    };
    let body = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    BaseError::ParamInvalid("body".to_string(), "请求体必须是 JSON".to_string()),
                )
            }
        }
    };

    let headers = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    let query = match parts.uri.query() {
        Some(raw) => match serde_urlencoded::from_str::<HashMap<String, String>>(raw) {
            Ok(query) => query,
            Err(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    BaseError::ParamInvalid("query".to_string(), "查询参数编码无效".to_string()),
                )
            }
        },
        None => HashMap::new(),
    };
    let action_request = Request::new(body)
        .headers(headers)
        .queries(query)
        .path_params(path_params);
    let request_meta = RequestMeta::new()
        .with_method(parts.method.to_string())
        .with_original_uri(parts.uri.to_string())
        .with_scheme(parts.uri.scheme_str().unwrap_or("http"))
        .with_peer_addr(peer)
        .with_local_addr(state.local_addr);
    let context = ActionContext::new(action_request, Arc::clone(&state.tools))
        .with_request_meta(request_meta);

    match state
        .app_router
        .dispatch(module_name, action_name, context)
        .await
    {
        Ok(response) => {
            let status = StatusCode::from_u16(success_status).unwrap_or(StatusCode::OK);
            (status, Json(response)).into_response()
        }
        Err(error) => {
            let status = status_for_error(&error);
            if status.is_server_error() {
                tracing::error!(error = %error, code = error.code(), "请求处理失败");
            }
            error_response(status, error)
        }
    }
}

async fn live() -> impl IntoResponse {
    Json(ApiResponse::success_value(
        json!({"status": "live"}),
        "服务存活",
    ))
}

async fn ready(State(state): State<HttpState>) -> Response {
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

fn method_filter(method: &str) -> anyhow::Result<MethodFilter> {
    match method {
        "GET" => Ok(MethodFilter::GET),
        "POST" => Ok(MethodFilter::POST),
        "PUT" => Ok(MethodFilter::PUT),
        "PATCH" => Ok(MethodFilter::PATCH),
        "DELETE" => Ok(MethodFilter::DELETE),
        "HEAD" => Ok(MethodFilter::HEAD),
        "OPTIONS" => Ok(MethodFilter::OPTIONS),
        "TRACE" => Ok(MethodFilter::TRACE),
        other => bail!("不支持的 HTTP 方法: {other}"),
    }
}

fn status_for_error(error: &BaseError) -> StatusCode {
    match error {
        BaseError::Unauthorized(_)
        | BaseError::InvalidPassword
        | BaseError::TokenKeyInvalid(_)
        | BaseError::TokenGenerateFailed(_)
        | BaseError::TokenVerifyFailed(_)
        | BaseError::TokenParseFailed(_)
        | BaseError::TokenExpired
        | BaseError::TokenRevoked
        | BaseError::TokenTypeInvalid(_) => StatusCode::UNAUTHORIZED,
        BaseError::PermissionDenied(_) | BaseError::FieldPermissionDenied(_, _, _) => {
            StatusCode::FORBIDDEN
        }
        _ => match error.category() {
            ErrorCategory::Client => StatusCode::BAD_REQUEST,
            ErrorCategory::Auth => StatusCode::UNAUTHORIZED,
            ErrorCategory::NotFound => StatusCode::NOT_FOUND,
            ErrorCategory::Conflict => StatusCode::CONFLICT,
            ErrorCategory::Transient => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCategory::Server => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

fn error_response(status: StatusCode, error: BaseError) -> Response {
    let response = if status.is_server_error() {
        let message = if status == StatusCode::SERVICE_UNAVAILABLE {
            "服务暂时不可用"
        } else {
            "服务器内部错误"
        };
        ApiResponse::fail(error.code(), message)
    } else {
        ApiResponse::from_error(&error)
    };
    (status, Json(response)).into_response()
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %error, "监听关闭信号失败");
    }
    tracing::info!("收到关闭信号");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::build_app_router;
    use crate::config::SecuritySettings;
    use axum::body::Body;
    use jsonwebtoken::Algorithm;
    use sqlx::mysql::MySqlPoolOptions;
    use tower::ServiceExt;
    use yang_base::token::TokenManager;

    #[test]
    fn maps_engine_error_categories_to_http_status() {
        assert_eq!(
            status_for_error(&BaseError::ParamInvalid("x".to_string(), "bad".to_string())),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_error(&BaseError::Unauthorized("login".to_string())),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_for_error(&BaseError::UserNotFound("1".to_string())),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn supports_catalog_http_methods() {
        assert_eq!(
            method_filter("GET").unwrap_or_else(|error| panic!("GET 应受支持: {error}")),
            MethodFilter::GET
        );
        assert!(method_filter("CONNECT").is_err());
    }

    #[tokio::test]
    async fn catalog_route_rejects_invalid_json_before_dispatch() {
        let pool = Arc::new(
            MySqlPoolOptions::new()
                .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
                .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}")),
        );
        let security = Arc::new(SecuritySettings {
            username_min_length: 3,
            username_max_length: 64,
            password_min_length: 10,
            password_max_length: 128,
        });
        let app_router = Arc::new(
            build_app_router(Arc::clone(&pool), security)
                .unwrap_or_else(|error| panic!("应用路由应构建成功: {error}")),
        );
        let token_manager = TokenManager::new_symmetric(
            "01234567890123456789012345678901",
            Algorithm::HS256,
            "test".to_string(),
            "test-api".to_string(),
            60,
            120,
        );
        let state = HttpState {
            app_router,
            tools: Arc::new(GlobalTools::new(token_manager)),
            pool,
            local_addr: "127.0.0.1:8080"
                .parse()
                .unwrap_or_else(|error| panic!("测试监听地址应有效: {error}")),
            max_body_bytes: 1024,
        };
        let router =
            build_router(state).unwrap_or_else(|error| panic!("HTTP 路由应构建成功: {error}"));
        let mut request = AxumRequest::builder()
            .method("POST")
            .uri("/api/v1/accounts/register")
            .header("content-type", "application/json")
            .body(Body::from("not-json"))
            .unwrap_or_else(|error| panic!("测试请求应构建成功: {error}"));
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:50000"
                .parse::<SocketAddr>()
                .unwrap_or_else(|error| panic!("测试对端地址应有效: {error}")),
        ));

        let response = router
            .oneshot(request)
            .await
            .unwrap_or_else(|error| panic!("HTTP 请求应返回响应: {error}"));

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
