//! 生成飞书用户身份授权链接（控制台「去授权」按钮的后端）。
//!
//! # 为什么由后端生成，而不是前端拼
//!
//! 授权页必须带一个**后端签发**的 `state`：回调要靠它防 CSRF——没有它，攻击者
//! 可以把**自己的**授权码塞进回调，让系统的用户身份绑到攻击者的飞书账号上。
//! 所以 state 只在这里签发（本端点有 `feishu.approval.write` 权限门），
//! 且 10 分钟有效、一次性（见 `domain::user_token`）。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::user_token;

mod codes {
    /// OAuth 未配置（缺 `oauth_redirect_uri` 或出站凭证）。
    pub(super) const OAUTH_NOT_CONFIGURED: i32 = 50302;
}

/// 取授权链接的输入：无参数。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthorizeUrlInput {}

impl ParamInput for AuthorizeUrlInput {
    fn params() -> Params {
        Params::new()
    }
}

/// 授权链接投影。
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AuthorizeUrl {
    /// 交给浏览器打开的飞书授权页地址。
    authorize_url: String,
}

/// 注册授权链接端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("oauth_authorize_url"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/oauth/authorize-url")
        .display_name("生成飞书用户授权链接")
        .description("生成飞书用户身份（OAuth）授权页链接，由管理员本人打开并完成授权")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: AuthorizeUrlInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(
            codes::OAUTH_NOT_CONFIGURED,
            "未配置 [feishu] 段，无法发起飞书授权",
        ));
    };
    let config = match user_token::oauth_config(settings) {
        Ok(config) => config,
        Err(message) => return Ok(ApiResponse::fail(codes::OAUTH_NOT_CONFIGURED, message)),
    };

    let state = user_token::mint_state(user_token::now_unix_secs());
    let authorize_url =
        user_token::build_authorize_url(&config.credentials.app_id, &config.redirect_uri, &state);
    ApiResponse::success(
        AuthorizeUrl { authorize_url },
        "请在新窗口完成飞书授权（10 分钟内有效）",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_carries_the_configured_redirect_and_a_fresh_state() {
        let state = user_token::mint_state(user_token::now_unix_secs());
        let url = user_token::build_authorize_url(
            "cli_x",
            "http://47.109.148.207:18654/api/v1/feishu/oauth/callback",
            &state,
        );
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F47.109.148.207%3A18654"),
            "{url}"
        );
        assert!(url.contains(&format!("state={state}")), "{url}");
    }
}
