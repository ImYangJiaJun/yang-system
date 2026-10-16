//! 飞书事件订阅 challenge 应答。
//!
//! 飞书在配置事件订阅 URL 时会发送 `url_verification` 请求，要求原样回显 `challenge`
//! 字段。本端点同时校验 `verification_token` 以确认请求来源合法。
//!
//! 响应不走框架包络：飞书要求 plain `{"challenge": "..."}`。
//!
//! # 加密支持
//!
//! 飞书配置了 Encrypt Key 后，所有推送（包括 challenge）都走 `{"encrypt":"..."}` 格式。
//! 本端点先用 `event_encrypt_key` 做 AES-256-CBC 解密，再从解密后的 JSON 提取字段。
//! 解密失败时返回错误响应（飞书会重试）。

use serde_json::Value;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto;
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
pub(super) fn register(
    module: ModuleSpec,
    context: FeishuContext,
    settings: &FeishuSettings,
) -> ModuleSpec {
    let Some(token) = settings.event_verification_token.as_deref() else {
        return module;
    };
    let verification_token = token.to_string();
    let encrypt_key = settings.event_encrypt_key.clone();
    module
        .action_fn(
            yang_base::action_name!("event_challenge"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    context.clone(),
                    verification_token.clone(),
                    encrypt_key.clone(),
                )
            },
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
///
/// # 事件回调兼容
///
/// 飞书开放平台的事件订阅 URL 只能配一个。当 challenge 端点和事件接收端点
/// 共用同一个 URL 时，飞书的 `event_callback` 推送也会打到本端点。
/// 检测到 `type == "event_callback"` 时，委托给事件接收 handler 处理。
pub(super) async fn handle(
    ctx: ActionContext,
    input: ChallengeRequest,
    context: FeishuContext,
    verification_token: String,
    encrypt_key: Option<String>,
) -> Result<ResponseBody, BaseError> {
    let raw_body = &input.0;
    tracing::debug!(body = %raw_body, "飞书 challenge 请求到达");

    // 解密或直接用明文
    let body = match raw_body.get("encrypt").and_then(Value::as_str) {
        Some(encrypted) => {
            let Some(key_raw) = encrypt_key.as_deref() else {
                tracing::error!("飞书发送了加密请求，但 event_encrypt_key 未配置");
                return raw_json(serde_json::json!({"error": "encrypt key not configured"}));
            };
            let key = crypto::derive_key(key_raw);
            match crypto::decrypt_bytes(encrypted, &key) {
                Ok(decrypted) => match serde_json::from_slice::<Value>(&decrypted) {
                    Ok(parsed) => {
                        tracing::debug!(decrypted = %parsed, "飞书 challenge 解密成功");
                        parsed
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "飞书 challenge 解密后不是合法 JSON");
                        return raw_json(
                            serde_json::json!({"error": "decrypt produced invalid json"}),
                        );
                    }
                },
                Err(e) => {
                    tracing::error!(error = %e, "飞书 challenge 解密失败");
                    return raw_json(serde_json::json!({"error": "decrypt failed"}));
                }
            }
        }
        None => raw_body.clone(),
    };

    // 从 body 提取字段
    let r#type = body.get("type").and_then(Value::as_str).unwrap_or("");
    let challenge = body.get("challenge").and_then(Value::as_str).unwrap_or("");
    let token = body.get("token").and_then(Value::as_str).unwrap_or("");

    // 事件回调委托：飞书把 event_callback 推到了 challenge URL，转交事件接收处理。
    if r#type == "event_callback" {
        tracing::debug!("检测到 event_callback 类型，委托给事件接收处理");
        return super::receive_approval_event::handle(
            ctx,
            super::receive_approval_event::EventRequest(body),
            context,
            Some(verification_token),
            encrypt_key,
        )
        .await;
    }

    // 缺少 challenge 字段 → 无法应答
    if challenge.is_empty() {
        tracing::warn!(body = %body, "飞书 challenge 请求缺少 challenge 字段");
        return raw_json(serde_json::json!({"error": "missing challenge"}));
    }

    // 类型校验
    if r#type != "url_verification" {
        tracing::warn!(body = %body, r#type = %r#type, "飞书 challenge 请求 type 不是 url_verification");
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
