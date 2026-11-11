//! 已绑定用户的飞书登录第二因子：TOTP / 恢复码 / 备用邮箱验证码。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::{cookie_secure, request_meta, session_json_response, state};
use crate::addon::account::ExternalLoginPort;
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) MfaInput {
        token: Str::new().require(true).max_length(128),
        mfa_code: Str::new().require(true).max_length(32),
    }
}

pub(super) fn register(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_mfa"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    Arc::clone(&context),
                    Arc::clone(&external_login),
                )
            },
        )
        .route(HttpMethod::Post, "/api/v1/auth/feishu/mfa")
        .display_name("飞书登录第二因子")
        .description("完成飞书登录的 TOTP/恢复码/备用邮箱验证码校验并签发会话")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: MfaInput,
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
    let Some(pending) = state::peek_pending_login(token, &nonce, now) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    match external_login
        .verify_second_factor(&ctx, pending.user_id, &input.mfa_code, "feishu_mfa")
        .await
    {
        Ok(()) => {}
        Err(BaseError::InvalidPassword) => {
            return Err(BaseError::ParamInvalid(
                "mfa_code".to_string(),
                "双重验证码错误或已过期".to_string(),
            ));
        }
        Err(error) => return Err(error),
    }
    let (ip, user_agent) = request_meta(&ctx);
    let session = external_login
        .issue_session(&ctx, pending.user_id, &ip, &user_agent)
        .await?;
    let _ = state::consume_pending_login(token, &nonce, now);
    session_json_response(
        session.access_token,
        &session.refresh_token,
        &pending.redirect,
        cookie_secure(settings),
    )
}
