//! 飞书事件订阅 challenge 应答。
//!
//! 飞书在配置事件订阅 URL 时会发送 `url_verification` 请求，要求原样回显 `challenge`
//! 字段。本端点同时校验 `verification_token` 以确认请求来源合法。
//!
//! 响应不走框架包络：飞书要求 plain `{"challenge": "..."}`。

use schemars::JsonSchema;
use serde::Deserialize;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::config::FeishuSettings;

/// challenge 请求体。
///
/// 不设 `deny_unknown_fields`：飞书可能加字段，容忍未知字段是飞书入站端点的统一策略
/// （见 `FeishuOptionsRequest`）。
#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ChallengeRequest {
    /// 固定为 `"url_verification"`。
    pub(super) r#type: String,
    /// 需要原样回显的挑战值。
    pub(super) challenge: String,
    /// 飞书侧的校验令牌，须与配置值一致。
    pub(super) token: String,
}

impl ParamInput for ChallengeRequest {
    fn params() -> Params {
        Params::new()
    }
}

/// 注册 challenge 应答端点。
pub(super) fn register(module: ModuleSpec, settings: &FeishuSettings) -> ModuleSpec {
    let Some(token) = settings.event_verification_token.as_deref() else {
        return module;
    };
    let token = token.to_string();
    module
        .action_fn(
            yang_base::action_name!("event_challenge"),
            move |ctx, input| handle(ctx, input, token.clone()),
        )
        .route(HttpMethod::Post, "/api/v1/feishu/event/challenge")
        .display_name("飞书事件 challenge")
        .description("飞书事件订阅 URL 验证的 challenge 应答")
        .public()
        .register()
}

/// 处理 challenge 请求。
///
/// 任何失败都返回 `Ok(ResponseBody::raw(...))`，不用 `Err`：匿名端点的 `Err`
/// 会消耗全站可用性预算（与 `approval_options` 同一纪律）。
pub(super) async fn handle(
    _ctx: ActionContext,
    input: ChallengeRequest,
    verification_token: String,
) -> Result<ResponseBody, BaseError> {
    // 类型校验
    if input.r#type != "url_verification" {
        return raw_json(serde_json::json!({"error": "unknown type"}));
    }
    // verification_token 校验
    if input.token != verification_token {
        tracing::warn!("飞书 challenge verification_token 不匹配");
        return raw_json(serde_json::json!({"error": "invalid token"}));
    }
    raw_json(serde_json::json!({"challenge": input.challenge}))
}

/// 构造 raw JSON 响应（不套框架包络）。
fn raw_json(value: serde_json::Value) -> Result<ResponseBody, BaseError> {
    let body =
        serde_json::to_string(&value).map_err(|e| BaseError::JsonSerializeFailed(e.to_string()))?;
    ResponseBody::raw(body, "application/json")
}
