//! 选择页「新建账号」：用户自行设置用户名/密码，创建后立刻绑定飞书身份。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::{cookie_secure, credentials, request_meta, session_json_response, state};
use crate::addon::account::ExternalLoginPort;
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) ChooseCreateInput {
        token: Str::new().require(true).max_length(128),
        username: Str::new().require(true).max_length(64),
        password: Str::new().require(true).max_length(128),
    }
}

pub(super) fn register(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_choose_create"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    Arc::clone(&context),
                    Arc::clone(&external_login),
                )
            },
        )
        .route(HttpMethod::Post, "/api/v1/auth/feishu/choose/create")
        .display_name("新建系统账号")
        .description("用用户输入的用户名/密码创建系统账号并绑定当前飞书身份")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ChooseCreateInput,
    context: Arc<FeishuContext>,
    external_login: Arc<dyn ExternalLoginPort>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书登录未启用"));
    };
    if !settings.login_allow_create {
        return Ok(ApiResponse::fail(
            40301,
            "当前部署不允许自助新建账号，请联系管理员",
        ));
    }
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
    if !external_login.has_system_owner(&ctx).await? {
        return Ok(ApiResponse::fail(
            50301,
            "系统尚未初始化，请先用账号密码注册首个管理员",
        ));
    }
    let (user_id, identity) = external_login
        .create_user_and_bind(&ctx, &input.username, &input.password, &pending.profile)
        .await?;
    if let Err(error) =
        credentials::upsert_credential(&ctx, &context, &identity, &pending.bundle, now).await
    {
        tracing::warn!(error = %error, "新建账号后写飞书凭证失败");
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
