//! 飞书用户身份授权回调：浏览器在授权页点完「授权」后被重定向到这里。
//!
//! # 这个端点为什么是 public
//!
//! 重定向由**浏览器**发起，带的是飞书的 `code` 与 `state`，没有本系统的 JWT。
//! 安全性靠两件事：`state` 必须由 [`super::oauth_authorize_url`] 签发过（CSRF 防护，
//! 见 `domain::user_token::consume_state`），以及授权码一次性、5 分钟有效。
//!
//! # 为什么返回 JSON 而不是一个漂亮的 HTML 页
//!
//! 框架的 `ResponseBody::Raw` 把 `content_type` 限死为 `application/json`
//! （`yang-base/src/action/response.rs:446` 明说「防止它被用来返回 text/html」），
//! 这是刻意的约束，不绕。所以这里返回带可读文案的 JSON 信封：管理员看到
//! 「飞书授权成功，可关闭本页」，回控制台点一下刷新即可。
//!
//! 端点**不回显任何入参**，也不把飞书的原始报文回给浏览器——失败详情只进日志，
//! 浏览器只看到本系统自己的可行动文案。

use std::sync::Arc;

use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::user_token;
use crate::infrastructure::audit;

mod codes {
    /// state 校验失败（未签发 / 已用过 / 已过期）。
    pub(super) const STATE_INVALID: i32 = 40301;
    /// 换 token 或取用户信息失败。
    pub(super) const EXCHANGE_FAILED: i32 = 40302;
    /// OAuth 未配置（缺 `oauth_redirect_uri` 或出站凭证）。
    pub(super) const OAUTH_NOT_CONFIGURED: i32 = 50302;
    /// 管理员在授权页点了拒绝。
    pub(super) const ACCESS_DENIED: i32 = 40303;
}

// 三个字段都走 **query**（浏览器重定向只能带查询串）。
// **刻意不加 `#[deny_unknown_fields]`**：这是浏览器与中间代理会经手的地址，
// 多一个无关参数不该让回调 400。
yang_base::params! {
    pub(super) OauthCallbackInput {
        /// 授权码；用户拒绝授权时不存在。
        #[param(source = query)]
        code: Str::new(),
        /// 回传的 state 原值。
        #[param(source = query)]
        state: Str::new(),
        /// 用户拒绝授权时飞书固定回 `access_denied`。
        #[param(source = query)]
        error: Str::new(),
    }
}

/// 注册授权回调端点。
///
/// 与派发端点同一条纪律：**只在集成可用时注册**（回调要打飞书的令牌端点）。
/// 凭证缺失时不注册，路由直接 404，而不是注册出来一个永远失败的端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("oauth_callback"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/feishu/oauth/callback")
        .display_name("飞书用户授权回调")
        .description("飞书授权页的重定向落点：校验 state、换取用户凭证并落库")
        .public()
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: OauthCallbackInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let now = user_token::now_unix_secs();

    // 用户在授权页点了拒绝：什么都没发生，给一句人话。
    if input.error.as_deref() == Some("access_denied") {
        return Ok(ApiResponse::fail(
            codes::ACCESS_DENIED,
            "已取消飞书授权（未做任何改动）",
        ));
    }

    // state 先校验、先消费：不通过就到此为止，不打任何出站请求。
    let state_ok = input
        .state
        .as_deref()
        .is_some_and(|state| user_token::consume_state(state, now));
    if !state_ok {
        tracing::warn!("飞书授权回调的 state 校验未通过（未签发 / 已用过 / 已过期）");
        return Ok(ApiResponse::fail(
            codes::STATE_INVALID,
            "授权链接已失效：请回到「飞书审批配置」页重新点「去授权」",
        ));
    }

    let Some(code) = input
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(ApiResponse::fail(
            codes::EXCHANGE_FAILED,
            "回调里没有授权码：请回到「飞书审批配置」页重新点「去授权」",
        ));
    };

    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(
            codes::OAUTH_NOT_CONFIGURED,
            "未配置 [feishu] 段，无法完成飞书授权",
        ));
    };
    let config = match user_token::oauth_config(settings) {
        Ok(config) => config,
        Err(message) => return Ok(ApiResponse::fail(codes::OAUTH_NOT_CONFIGURED, message)),
    };

    let outbound = outbound_setup::build(&ctx, settings)?;
    let bundle = match user_token::exchange_code(
        outbound.transport(),
        outbound.sleeper(),
        &config.credentials,
        code,
        &config.redirect_uri,
        now,
    )
    .await
    {
        Ok(bundle) => bundle,
        Err(error) => return Ok(ApiResponse::fail(codes::EXCHANGE_FAILED, describe(&error))),
    };

    let (open_id, name) = match user_token::fetch_user_info(
        outbound.transport(),
        outbound.sleeper(),
        &bundle.access_token,
    )
    .await
    {
        Ok(info) => info,
        Err(error) => return Ok(ApiResponse::fail(codes::EXCHANGE_FAILED, describe(&error))),
    };

    user_token::store_authorization(
        context.user_tokens(),
        &open_id,
        name.as_deref(),
        &bundle,
        now,
    )
    .await?;
    tracing::info!(
        open_id = %open_id,
        name = name.as_deref().unwrap_or_default(),
        "飞书用户身份授权完成"
    );
    audit_authorization(&ctx, &open_id, &bundle, now).await;

    ApiResponse::success(
        serde_json::json!({
            "name": name,
            "authorized_at": now,
            "scope": bundle.scope,
        }),
        "飞书授权成功，可关闭本页",
    )
}

/// 把令牌端点的失败折成**浏览器里读得懂**的一句人话（含排查提示与业务码）。
fn describe(error: &user_token::TokenEndpointError) -> String {
    let hint = error.hint().unwrap_or_default();
    if hint.is_empty() {
        format!(
            "飞书授权换取凭证失败（错误码 {}）：{}",
            error.code, error.message
        )
    } else {
        format!(
            "飞书授权换取凭证失败（错误码 {}）：{}；{hint}",
            error.code, error.message
        )
    }
}

/// 记一条审计：授权是**凭据授予**，与改配置同一量级，必须留痕。
///
/// 审计写失败只记日志、不影响授权结果——凭证已经落库，回滚它没有意义。
async fn audit_authorization(
    ctx: &ActionContext,
    open_id: &str,
    bundle: &user_token::TokenBundle,
    now: i64,
) {
    let event = audit::succeeded_system_event_without_ctx(
        "feishu-oauth",
        "feishu.oauth_authorize",
        None,
        match audit::entity("feishu_user_token", open_id) {
            Ok(entity) => entity,
            Err(error) => {
                tracing::error!(error = %error, "飞书授权审计目标构造失败");
                return;
            }
        },
        match audit::summary([
            ("open_id", serde_json::json!(open_id)),
            ("authorized_at", serde_json::json!(now)),
            ("scope", serde_json::json!(bundle.scope.clone())),
        ]) {
            Ok(summary) => summary,
            Err(error) => {
                tracing::error!(error = %error, "飞书授权审计摘要构造失败");
                return;
            }
        },
    );
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            tracing::error!(error = %error, "飞书授权审计事件构造失败");
            return;
        }
    };
    match ctx.tools().mysql() {
        Ok(database) => {
            if let Err(error) = audit::append_independent(database.pool(), &event).await {
                tracing::error!(error = %error, "飞书授权审计写入失败");
            }
        }
        Err(error) => tracing::error!(error = %error, "飞书授权审计取库失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn callback_reads_code_state_and_error_from_the_query_string() {
        let mut request = yang_base::action::Request::new(serde_json::Value::Null)
            .query("code", "code-1")
            .query("state", "state-1");

        let input = OauthCallbackInput::decode(&mut request)
            .unwrap_or_else(|error| panic!("query 参数应可解码: {error}"));
        assert_eq!(input.code.as_deref(), Some("code-1"));
        assert_eq!(input.state.as_deref(), Some("state-1"));
        assert!(input.error.is_none(), "未传的字段应为 None");
    }

    #[test]
    fn callback_tolerates_extra_query_parameters() {
        // 浏览器与中间代理可能加参数：多一个不该让回调 400。
        let mut request = yang_base::action::Request::new(serde_json::Value::Null)
            .query("code", "code-1")
            .query("state", "state-1")
            .query("from", "browser");

        assert!(OauthCallbackInput::decode(&mut request).is_ok());
    }

    #[test]
    fn exchange_failures_are_described_with_code_and_hint() {
        let described = describe(&user_token::TokenEndpointError {
            code: 20071,
            message: "redirect_uri mismatch".to_string(),
        });
        assert!(described.contains("20071"), "{described}");
        assert!(described.contains("重定向"), "{described}");
    }
}
