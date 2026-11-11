//! 开始飞书 SSO 登录：生成 state/nonce/PKCE 并 302 到飞书授权页。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::{
    cookie_secure, error_redirect, login_config, redirect_response, state, validate_redirect,
};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::user_token;

yang_base::params! {
    pub(super) AuthorizeInput {
        #[param(source = query)]
        redirect: Str::new(),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_authorize"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/auth/feishu/authorize")
        .display_name("飞书登录")
        .description("开始飞书 OAuth 登录（state + nonce + PKCE）")
        .public()
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: AuthorizeInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings() else {
        return error_redirect("login_disabled");
    };
    if !settings.login_configured() {
        return error_redirect("login_disabled");
    }
    let redirect = match validate_redirect(settings, input.redirect.as_deref()) {
        Ok(redirect) => redirect,
        Err(_) => return error_redirect("invalid_redirect"),
    };
    let config = match login_config(settings) {
        Ok(config) => config,
        Err(_) => return error_redirect("login_disabled"),
    };
    let now = user_token::now_unix_secs();
    let (verifier, challenge) = state::pkce_pair();
    let nonce = state::random_token();
    let state = match state::mint_login_state(&redirect, &nonce, &verifier, now) {
        Ok(state) => state,
        Err(_) => return error_redirect("rate_limited"),
    };
    let url = user_token::build_sso_authorize_url(
        &config.credentials.app_id,
        &config.redirect_uri,
        &state,
        &challenge,
    );
    redirect_response(
        &url,
        Some(&state::nonce_cookie(&nonce, cookie_secure(settings))),
    )
}
