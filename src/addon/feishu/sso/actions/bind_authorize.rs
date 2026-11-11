//! 已登录用户开始绑定飞书：签发 Bind flow 的 state 并返回授权 URL。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::{cookie_secure, login_config, require_current_user, state, validate_redirect};
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::user_token;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BindAuthorizeInput {
    /// 显式二次确认；前端在跳转授权页前让用户确认。
    confirm: String,
    /// 当前账号密码，用作 Step-up。
    password: String,
}

impl ParamInput for BindAuthorizeInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能访问自己的飞书身份与批量审批数据
    module
        .action_fn(
            yang_base::action_name!("feishu_bind_authorize"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/users/me/identities/feishu/authorize",
        )
        .display_name("绑定飞书身份")
        .description("生成飞书授权 URL，把当前飞书身份绑定到当前系统账号")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: BindAuthorizeInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    if input.confirm != "true" {
        return Err(BaseError::ParamInvalid(
            "confirm".to_string(),
            "请先确认绑定飞书身份".to_string(),
        ));
    }
    let user_id = require_current_user(&ctx)?;
    if input.password.is_empty() {
        return Err(BaseError::ParamInvalid(
            "password".to_string(),
            "请输入当前账号密码以确认身份".to_string(),
        ));
    }
    let Some(external_login) = context.external_login() else {
        return Ok(ApiResponse::fail(50301, "飞书登录未启用"));
    };
    external_login
        .verify_current_password(&ctx, user_id, &input.password)
        .await?;
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书登录未启用"));
    };
    if !settings.login_configured() {
        return Ok(ApiResponse::fail(50301, "飞书登录未启用"));
    }
    let config = match login_config(settings) {
        Ok(config) => config,
        Err(message) => return Ok(ApiResponse::fail(50301, message)),
    };
    let redirect = match validate_redirect(settings, Some("/account")) {
        Ok(redirect) => redirect,
        Err(_) => "/".to_string(),
    };
    let now = user_token::now_unix_secs();
    let (verifier, challenge) = state::pkce_pair();
    let nonce = state::random_token();
    let state_value = state::mint_bind_state(user_id, &redirect, &nonce, &verifier, now)?;
    let url = user_token::build_sso_authorize_url(
        &config.credentials.app_id,
        &config.redirect_uri,
        &state_value,
        &challenge,
    );
    ApiResponse::success(
        serde_json::json!({ "authorize_url": url }),
        "请继续在飞书中完成授权",
    )?
    .with_header(
        "set-cookie",
        state::nonce_cookie(&nonce, cookie_secure(settings)),
    )
}
