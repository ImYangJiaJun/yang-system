//! 飞书 SSO（用户登录/绑定）module。
//!
//! 本 module 不声明表：身份映射在 account 的 `user_identity`，令牌在
//! `feishu_user_credential`（纯表 module），账号域通过 `ExternalLoginPort` 暴露
//! 查找/建号/绑定/签发会话能力。

mod actions;
pub(crate) mod domain;
pub(crate) use domain::{credentials, state};

use std::sync::Arc;

use serde_json::json;

use yang_base::action::{ApiResponse, ResponseAttachment, TokenAuthMiddleware};
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

use crate::addon::account::{user_from_claims, Account, ExternalLoginPort};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::tenant_token::FeishuCredentials;
use crate::addon::feishu::domain::user_token::{self, OAuthConfig, TokenBundle, UserInfo};
use crate::authorization::AuthorizationVersionValidator;
use crate::config::FeishuSettings;

pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let module = ModuleSpec::new(
        ModuleName::new("feishu.sso").map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    Ok(actions::register_all(module, context, external_login))
}

/// 登录专用 OAuth 配置：不要求 `can_pull()`。
pub(crate) fn login_config(settings: &FeishuSettings) -> Result<OAuthConfig, String> {
    if !settings.login_configured() {
        return Err("飞书登录未启用或缺少 app_id/app_secret/login_redirect_uri".to_string());
    }
    let redirect_uri = settings
        .login_redirect_uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 feishu.login_redirect_uri".to_string())?;
    Ok(OAuthConfig {
        credentials: FeishuCredentials {
            app_id: settings.app_id.clone().unwrap_or_default(),
            app_secret: settings.app_secret.clone().unwrap_or_default(),
        },
        redirect_uri: redirect_uri.to_string(),
    })
}

pub(crate) fn cookie_secure(settings: &FeishuSettings) -> bool {
    settings.login_cookie_secure()
}

/// 只允许配置白名单内的站内相对路径。
pub(crate) fn validate_redirect(
    settings: &FeishuSettings,
    redirect: Option<&str>,
) -> Result<String, String> {
    let redirect = redirect
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("/");
    if !redirect.starts_with('/')
        || redirect.starts_with("//")
        || redirect.contains('\\')
        || redirect.contains("://")
    {
        return Err("redirect 必须是站内相对路径".to_string());
    }
    if !settings
        .login_allowed_redirects
        .iter()
        .any(|allowed| allowed == redirect)
    {
        return Err("redirect 不在允许的回跳路径内".to_string());
    }
    Ok(redirect.to_string())
}

pub(crate) fn redirect_response(
    location: &str,
    set_cookie: Option<&str>,
) -> Result<ApiResponse, BaseError> {
    let mut response = ApiResponse::attachment(
        ResponseAttachment::Redirect {
            url: location.to_string(),
        },
        "跳转中",
    )
    .with_header("cache-control", "no-store")?
    .with_header("pragma", "no-cache")?;
    if let Some(cookie) = set_cookie {
        response = response.with_header("set-cookie", cookie)?;
    }
    Ok(response)
}

pub(crate) fn error_redirect(code: &str) -> Result<ApiResponse, BaseError> {
    redirect_response(&format!("/login?feishu_error={code}"), None)
}

pub(crate) fn account_error_redirect(code: &str) -> Result<ApiResponse, BaseError> {
    redirect_response(&format!("/account?feishu_error={code}"), None)
}

pub(crate) fn account_ok_redirect() -> Result<ApiResponse, BaseError> {
    redirect_response("/account?feishu=bound", None)
}

/// 登录/绑定成功后的 JSON 响应：access token 进响应体，refresh 进 Cookie。
pub(crate) fn session_json_response(
    access_token: String,
    refresh_token: &str,
    redirect: &str,
    secure: bool,
) -> Result<ApiResponse, BaseError> {
    let response = ApiResponse::success(
        json!({ "access_token": access_token, "redirect": redirect }),
        "会话已建立",
    )?
    .with_header(
        "set-cookie",
        Account::refresh_cookie_header(refresh_token, secure),
    )?
    .with_header("cache-control", "no-store")?;
    response.with_header("pragma", "no-cache")
}

pub(crate) fn mask_email(email: Option<&str>) -> Option<String> {
    let email = email?.trim();
    let (local, domain) = email.split_once('@')?;
    let first = local.chars().next()?;
    Some(format!("{first}***@{domain}"))
}

pub(crate) fn request_meta(ctx: &yang_base::action::ActionContext) -> (String, String) {
    let ip = yang_base::transport::client_ip::client_ip_identity(ctx).into_owned();
    let user_agent = ctx
        .request
        .get_header("user-agent")
        .unwrap_or_default()
        .to_string();
    (ip, user_agent)
}

/// 换码 + 取完整用户信息。
pub(crate) async fn exchange_and_profile(
    ctx: &yang_base::action::ActionContext,
    settings: &FeishuSettings,
    code: &str,
    code_verifier: &str,
) -> Result<(TokenBundle, UserInfo), String> {
    let config = login_config(settings)?;
    let outbound = outbound_setup::build(ctx, settings).map_err(|error| error.to_string())?;
    let now = user_token::now_unix_secs();
    let bundle = user_token::exchange_code_pkce(
        outbound.transport(),
        outbound.sleeper(),
        &config.credentials,
        code,
        &config.redirect_uri,
        code_verifier,
        now,
    )
    .await
    .map_err(|error| describe_token_error(&error))?;
    let info = user_token::fetch_user_info_full(
        outbound.transport(),
        outbound.sleeper(),
        &bundle.access_token,
    )
    .await
    .map_err(|error| describe_token_error(&error))?;
    Ok((bundle, info))
}

pub(crate) fn describe_token_error(error: &user_token::TokenEndpointError) -> String {
    let hint = error.hint().unwrap_or_default();
    if hint.is_empty() {
        format!("飞书授权失败（错误码 {}）：{}", error.code, error.message)
    } else {
        format!(
            "飞书授权失败（错误码 {}）：{}；{hint}",
            error.code, error.message
        )
    }
}

pub(crate) fn require_current_user(
    ctx: &yang_base::action::ActionContext,
) -> Result<i64, BaseError> {
    ctx.authenticated_user()
        .map(|user| user.id)
        .ok_or_else(|| BaseError::Unauthorized("需要登录".to_string()))
}
