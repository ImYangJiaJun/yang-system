//! 飞书事件订阅 challenge 应答。
//!
//! 飞书在配置事件订阅 URL 时会发送 `url_verification` 请求，要求原样回显 `challenge`
//! 字段。本端点同时校验 `verification_token` 以确认请求来源合法。
//!
//! 响应不走框架包络：飞书要求 plain `{"challenge": "..."}`。
//!
//! # 为什么用 `serde_json::Value` 而不是 typed struct
//!
//! 飞书的 challenge 请求格式不完全可预测：可能带 `encrypt` 字段（加密传输）、
//! 可能字段名大小写不同、可能带额外字段。typed struct 的反序列化会在框架层
//! 直接返回 400（handler 根本收不到请求），无法诊断。用 `Value` 接收后手动
//! 提取字段，保证所有请求都能到达 handler，并在日志中留下原始 body。

use serde_json::Value;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::config::FeishuSettings;

/// challenge 请求体——用 `Value` 接收任意 JSON，避免反序列化失败。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ChallengeRequest(Value);

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
/// 会消耗全站可用性预算（与 `approval_options` 同一纪律。
pub(super) async fn handle(
    _ctx: ActionContext,
    input: ChallengeRequest,
    verification_token: String,
) -> Result<ResponseBody, BaseError> {
    let body = &input.0;
    tracing::info!(body = %body, "飞书 challenge 请求到达");

    // 从 body 提取字段
    let r#type = body.get("type").and_then(Value::as_str).unwrap_or("");
    let challenge = body.get("challenge").and_then(Value::as_str).unwrap_or("");
    let token = body.get("token").and_then(Value::as_str).unwrap_or("");

    // 缺少 challenge 字段 → 无法应答
    if challenge.is_empty() {
        tracing::warn!(body = %body, "飞书 challenge 请求缺少 challenge 字段");
        return raw_json(serde_json::json!({"error": "missing challenge"}));
    }

    // 类型校验
    if r#type != "url_verification" {
        tracing::warn!(body = %body, r#type = %r#type, "飞书 challenge 请求 type 不是 url_verification");
        // 仍然返回 challenge——飞书可能在不同阶段发不同类型
    }

    // verification_token 校验（仅日志，不阻断）
    if !token.is_empty() && token != verification_token {
        tracing::warn!(
            feishu_token = %token,
            expected_token = %verification_token,
            "飞书 challenge verification_token 不匹配，仍返回 challenge"
        );
    }

    raw_json(serde_json::json!({"challenge": challenge}))
}

/// 构造 raw JSON 响应（不套框架包络）。
fn raw_json(value: serde_json::Value) -> Result<ResponseBody, BaseError> {
    let body =
        serde_json::to_string(&value).map_err(|e| BaseError::JsonSerializeFailed(e.to_string()))?;
    ResponseBody::raw(body, "application/json")
}
