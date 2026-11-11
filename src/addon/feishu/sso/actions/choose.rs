//! 首次飞书登录的账号选择页数据（只读、不消费 pending token）。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::{mask_email, state};
use crate::addon::feishu::domain::context::FeishuContext;

yang_base::params! {
    pub(super) ChooseInput {
        #[param(source = query)]
        token: Str::new(),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("feishu_sso_choose"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/auth/feishu/choose")
        .display_name("飞书账号选择")
        .description("展示待绑定的飞书身份与「绑定已有 / 新建账号」选项")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: ChooseInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书登录未启用"));
    };
    let Some(token) = input.token.as_deref().filter(|value| !value.is_empty()) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    let Some(nonce) = state::nonce_from_request(&ctx) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    let now = crate::addon::feishu::domain::user_token::now_unix_secs();
    let Some(pending) = state::peek_choice(token, &nonce, now) else {
        return Ok(ApiResponse::fail(40101, "登录已过期，请重新点飞书登录"));
    };
    ApiResponse::success(
        serde_json::json!({
            "name": pending.profile.name,
            "email_masked": mask_email(pending.profile.email.as_deref()),
            "email_matches_existing": pending.email_matches_existing,
            "allow_create": settings.login_allow_create,
            "redirect": pending.redirect,
        }),
        "ok",
    )
}
