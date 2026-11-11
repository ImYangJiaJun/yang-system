//! 飞书 SSO 回调：校验 state/nonce、换码、查/建/绑、签发会话或跳选择页/MFA 页。
//!
//! 这是浏览器跨站顶层导航的落点，**不能**调用 `BrowserSession::validate_same_origin`
//! （飞书的 `Sec-Fetch-Site: cross-site` 会被拒）；CSRF 由一次性的 state 与
//! 发起登录时种下的 nonce Cookie 共同防护。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::{
    account_error_redirect, account_ok_redirect, cookie_secure, credentials, error_redirect,
    exchange_and_profile, redirect_response, state,
};
use crate::addon::account::{Account, ExternalIdentityProfile, ExternalLoginPort, PROVIDER_FEISHU};
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) CallbackInput {
        #[param(source = query)]
        code: Str::new(),
        #[param(source = query)]
        state: Str::new(),
        #[param(source = query)]
        error: Str::new(),
    }
}

pub(super) fn register(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_callback"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    Arc::clone(&context),
                    Arc::clone(&external_login),
                )
            },
        )
        .route(HttpMethod::Get, "/api/v1/auth/feishu/callback")
        .display_name("飞书登录回调")
        .description("飞书授权页的重定向落点：校验 state/nonce、换码并登录或进入账号选择")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: CallbackInput,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings() else {
        return error_redirect("login_disabled");
    };
    if !settings.login_configured() {
        return error_redirect("login_disabled");
    }
    let now = crate::addon::feishu::domain::user_token::now_unix_secs();
    let Some(nonce) = state::nonce_from_request(&ctx) else {
        return error_redirect("login_expired");
    };
    let Some(state_value) = input
        .state
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return error_redirect("login_expired");
    };
    let Some(record) = state::consume_state(state_value, now) else {
        tracing::warn!("飞书 SSO 回调的 state 校验未通过（未签发 / 已用过 / 已过期）");
        return error_redirect("login_expired");
    };
    if record.nonce_hash != state::hash_nonce(&nonce) {
        tracing::warn!("飞书 SSO 回调的 nonce 不匹配");
        return error_redirect("login_expired");
    }
    if input.error.as_deref() == Some("access_denied") {
        return error_redirect("access_denied");
    }
    let Some(code) = input
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return error_redirect("missing_code");
    };
    let (bundle, info) =
        match exchange_and_profile(&ctx, settings, code, &record.code_verifier).await {
            Ok(value) => value,
            Err(message) => {
                tracing::warn!(error = %message, "飞书 SSO 换码/取用户信息失败");
                return error_redirect("exchange_failed");
            }
        };
    let tenant_key = info.tenant_key.clone().unwrap_or_default();
    if !settings.login_allowed_tenant_keys.is_empty()
        && !settings
            .login_allowed_tenant_keys
            .iter()
            .any(|allowed| allowed == &tenant_key)
    {
        return error_redirect("tenant_rejected");
    }
    let profile = ExternalIdentityProfile::feishu(
        tenant_key,
        info.open_id.clone(),
        info.union_id.clone(),
        info.name.clone(),
        info.email.clone(),
        info.avatar_url.clone(),
    );

    match record.flow {
        state::StateFlow::Bind { user_id } => {
            let identity = match external_login.bind_identity(&ctx, user_id, &profile).await {
                Ok(identity) => identity,
                Err(error) => {
                    tracing::warn!(error = %error, user_id, "飞书绑定已有账号失败");
                    return account_error_redirect("bind_failed");
                }
            };
            if let Err(error) =
                credentials::upsert_credential(&ctx, &context, &identity, &bundle, now).await
            {
                tracing::warn!(error = %error, "飞书绑定后写凭证失败");
            }
            if let Err(error) = external_login
                .touch_identity_login(&ctx, identity.id, &profile)
                .await
            {
                tracing::warn!(error = %error, "飞书绑定后更新登录时间失败");
            }
            account_ok_redirect()
        }
        state::StateFlow::Login => {
            if let Some(identity) = external_login
                .find_identity(
                    &ctx,
                    PROVIDER_FEISHU,
                    &profile.tenant_key,
                    &profile.external_id,
                )
                .await?
            {
                if !external_login
                    .user_is_active(&ctx, identity.user_id)
                    .await?
                {
                    return error_redirect("account_disabled");
                }
                if let Err(error) =
                    credentials::upsert_credential(&ctx, &context, &identity, &bundle, now).await
                {
                    tracing::warn!(error = %error, "飞书登录后写凭证失败");
                }
                if let Err(error) = external_login
                    .touch_identity_login(&ctx, identity.id, &profile)
                    .await
                {
                    tracing::warn!(error = %error, "飞书登录后更新登录时间失败");
                }
                let (ip, user_agent) = super::super::request_meta(&ctx);
                if external_login
                    .user_requires_second_factor(&ctx, identity.user_id)
                    .await?
                {
                    let token =
                        state::mint_pending_login(identity.user_id, &record.redirect, &nonce, now)?;
                    return redirect_response(&format!("/login/feishu/mfa?token={token}"), None);
                }
                let session = external_login
                    .issue_session(&ctx, identity.user_id, &ip, &user_agent)
                    .await?;
                Account::session_redirect_response(
                    &record.redirect,
                    &session.refresh_token,
                    cookie_secure(settings),
                )
            } else {
                if !settings.login_allow_create {
                    return error_redirect("no_account");
                }
                if !external_login.has_system_owner(&ctx).await? {
                    return error_redirect("not_initialized");
                }
                let email_matches_existing = match info.email.as_deref() {
                    Some(email) => external_login
                        .find_active_user_by_email(&ctx, email)
                        .await?
                        .is_some(),
                    None => false,
                };
                let token = state::mint_pending_choice(
                    profile,
                    bundle,
                    &record.redirect,
                    &nonce,
                    email_matches_existing,
                    now,
                )?;
                redirect_response(&format!("/login/feishu/choose?token={token}"), None)
            }
        }
    }
}
