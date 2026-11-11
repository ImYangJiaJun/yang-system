//! 选择页「绑定已有账号」：账号标识 + 密码（+MFA）→ 绑定并签发会话。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::{cookie_secure, credentials, request_meta, session_json_response, state};
use crate::addon::account::ExternalLoginPort;
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ChooseBindInput {
        token: Str::new().require(true).max_length(128),
        username: Str::new().require(true).max_length(254),
        password: Str::new().require(true).max_length(128),
        mfa_code: Str::new().max_length(32),
    }
}

pub(super) fn register(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_choose_bind"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    Arc::clone(&context),
                    Arc::clone(&external_login),
                )
            },
        )
        .route(HttpMethod::Post, "/api/v1/auth/feishu/choose/bind")
        .display_name("绑定已有账号")
        .description("用已有系统账号的用户名/邮箱 + 密码（+MFA）绑定当前飞书身份")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ChooseBindInput,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书登录未启用"));
    };
    let Some(nonce) = state::nonce_from_request(&ctx) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    let token = input.token.as_str();
    if token.is_empty() {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    }
    let now = crate::addon::feishu::domain::user_token::now_unix_secs();
    let Some(pending) = state::peek_choice(token, &nonce, now) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    let user_id = external_login
        .verify_password(
            &ctx,
            &input.username,
            &input.password,
            input.mfa_code.as_deref(),
        )
        .await?;
    let identity = external_login
        .bind_identity(&ctx, user_id, &pending.profile)
        .await?;
    if let Err(error) =
        credentials::upsert_credential(&ctx, &context, &identity, &pending.bundle, now).await
    {
        tracing::warn!(error = %error, "绑定后写飞书凭证失败");
    }
    let (ip, user_agent) = request_meta(&ctx);
    let session = external_login
        .issue_session(&ctx, user_id, &ip, &user_agent)
        .await?;
    let _ = state::consume_choice(token, &nonce, now);
    session_json_response(
        session.access_token,
        &session.refresh_token,
        &pending.redirect,
        cookie_secure(settings),
    )
}
