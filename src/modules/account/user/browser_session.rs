//! 浏览器刷新会话的 Cookie 与同源边界。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::{ApiResponse, Request};
use yang_base::BaseError;

const REFRESH_COOKIE: &str = "yang_refresh";
const COOKIE_PATH: &str = "/api/v1/users";

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BrowserAccessToken {
    pub(super) access_token: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ReloginRequired {
    pub(super) relogin_required: bool,
}

pub(super) fn refresh_token(request: &Request) -> Result<String, BaseError> {
    request
        .cookie()
        .and_then(|header| {
            header.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                (name == REFRESH_COOKIE && !value.is_empty()).then(|| value.to_string())
            })
        })
        .ok_or_else(|| BaseError::Unauthorized("刷新会话 Cookie 缺失".to_string()))
}

pub(super) fn validate_same_origin(request: &Request) -> Result<bool, BaseError> {
    if request
        .get_header("sec-fetch-site")
        .is_some_and(|value| !matches!(value, "same-origin" | "none"))
    {
        return Err(BaseError::PermissionDenied(
            "浏览器会话请求必须来自同源页面".to_string(),
        ));
    }

    let source = request
        .get_header("origin")
        .or_else(|| request.get_header("referer"));
    let Some(source) = source else {
        // 非浏览器客户端通常没有 Origin/Referer；Cookie 不会被浏览器自动附带，
        // 因而不具备跨站请求伪造条件。
        return Ok(false);
    };
    let host = request
        .get_header("host")
        .ok_or_else(|| BaseError::PermissionDenied("同源校验缺少 Host".to_string()))?;
    let uri = source
        .parse::<axum::http::Uri>()
        .map_err(|_| BaseError::PermissionDenied("Origin/Referer 非法".to_string()))?;
    let source_host = uri
        .authority()
        .map(|authority| authority.as_str())
        .ok_or_else(|| BaseError::PermissionDenied("Origin/Referer 缺少主机".to_string()))?;
    if !source_host.eq_ignore_ascii_case(host.trim()) {
        return Err(BaseError::PermissionDenied(
            "浏览器会话请求必须来自同源页面".to_string(),
        ));
    }
    Ok(uri.scheme_str() == Some("https"))
}

pub(super) fn token_response(
    access_token: String,
    refresh_token: String,
    secure: bool,
) -> Result<ApiResponse, BaseError> {
    no_store(
        ApiResponse::success(BrowserAccessToken { access_token }, "会话已建立")?
            .with_header("set-cookie", refresh_cookie(&refresh_token, secure))?,
    )
}

pub(super) fn clear_response(
    response: ApiResponse,
    secure: bool,
) -> Result<ApiResponse, BaseError> {
    no_store(response.with_header("set-cookie", clear_refresh_cookie(secure))?)
}

pub(super) fn relogin_response(message: &str, secure: bool) -> Result<ApiResponse, BaseError> {
    clear_response(
        ApiResponse::success(
            ReloginRequired {
                relogin_required: true,
            },
            message,
        )?,
        secure,
    )
}

fn no_store(response: ApiResponse) -> Result<ApiResponse, BaseError> {
    response
        .with_header("cache-control", "no-store")?
        .with_header("pragma", "no-cache")
}

fn refresh_cookie(token: &str, secure: bool) -> String {
    format!(
        "{REFRESH_COOKIE}={token}; Path={COOKIE_PATH}; HttpOnly; SameSite=Strict{}",
        if secure { "; Secure" } else { "" }
    )
}

fn clear_refresh_cookie(secure: bool) -> String {
    format!(
        "{REFRESH_COOKIE}=; Path={COOKIE_PATH}; HttpOnly; SameSite=Strict; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn refresh_cookie_is_http_only_strict_and_host_only() {
        let cookie = refresh_cookie("secret", true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("Path=/api/v1/users"));
        assert!(!cookie.contains("Domain="));
    }

    #[test]
    fn browser_posts_require_exact_origin() {
        let mut headers = HashMap::new();
        headers.insert("host".to_string(), "app.example.com".to_string());
        headers.insert("origin".to_string(), "https://app.example.com".to_string());
        headers.insert("sec-fetch-site".to_string(), "same-origin".to_string());
        assert!(validate_same_origin(
            &Request::new(serde_json::json!({})).headers(headers.clone())
        )
        .unwrap_or(false));
        headers.insert("origin".to_string(), "https://evil.example.com".to_string());
        assert!(
            validate_same_origin(&Request::new(serde_json::json!({})).headers(headers)).is_err()
        );
    }
}
