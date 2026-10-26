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
use crate::addon::feishu::domain::token::tokens_equal;
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
/// 配置/密钥故障返回服务端错误，非法格式返回客户端错误，不能伪装成成功 ACK；
/// verification_token 缺失或不匹配返回 403，**不再回显 challenge**。
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

    // 解密或直接用明文
    let body = match raw_body.get("encrypt").and_then(Value::as_str) {
        Some(encrypted) => {
            let Some(key_raw) = encrypt_key.as_deref() else {
                tracing::error!("飞书发送了加密请求，但 event_encrypt_key 未配置");
                return Err(BaseError::ConfigError("飞书事件加密密钥未配置".into()));
            };
            let key = crypto::derive_key(key_raw);
            match crypto::decrypt_bytes(encrypted, &key) {
                Ok(decrypted) => match serde_json::from_slice::<Value>(&decrypted) {
                    Ok(parsed) => {
                        tracing::debug!(
                            decrypted_size = decrypted.len(),
                            "飞书 challenge 解密成功"
                        );
                        parsed
                    }
                    Err(e) => {
                        tracing::error!(category = ?e.classify(), line = e.line(), column = e.column(), "飞书 challenge 解密后不是合法 JSON");
                        return Err(BaseError::ValidationFailed(
                            "decrypted_body".into(),
                            "飞书事件解密后不是合法 JSON".into(),
                        ));
                    }
                },
                Err(e) => {
                    tracing::error!(error = %e, "飞书 challenge 解密失败");
                    return Err(BaseError::ConfigError(
                        "飞书事件解密失败，请检查密钥配置".into(),
                    ));
                }
            }
        }
        None => {
            if encrypt_key.is_some() {
                return Err(BaseError::Unauthorized(
                    "已配置 event_encrypt_key，拒绝明文 challenge".to_string(),
                ));
            }
            raw_body.clone()
        }
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
            // `body` 已在本函数解密（或本来就是无需解密的明文）；事件接收 handler
            // 若再看到 `encrypt_key = Some`，会把「没有 encrypt 字段」误判为明文
            // 注入并 401。这里传入 None 表示“已完成解密/校验”，不能把配置的 key
            // 继续往下传——否则「共用订阅 URL + Encrypt Key」会让所有事件被拒。
            None,
        )
        .await;
    }

    // 缺少 challenge 字段 → 无法应答
    if challenge.is_empty() {
        tracing::warn!("飞书 challenge 请求缺少 challenge 字段");
        return Err(BaseError::ValidationFailed(
            "challenge".into(),
            "缺少 challenge".into(),
        ));
    }

    // 类型校验
    if r#type != "url_verification" {
        tracing::warn!("飞书 challenge 请求 type 不是 url_verification");
        return Err(BaseError::ValidationFailed(
            "type".into(),
            "不支持的飞书回调类型".into(),
        ));
    }

    // verification_token 校验（fail-closed）：缺失或不匹配一律 403。
    if token.is_empty() || !tokens_equal(token, &verification_token) {
        tracing::warn!("飞书 challenge verification_token 缺失或不匹配，拒绝应答");
        return Err(BaseError::PermissionDenied(
            "飞书 challenge verification_token 无效".to_string(),
        ));
    }

    raw_json(serde_json::json!({"challenge": challenge}))
}

/// 构造 raw JSON 响应（不套框架包络）。
fn raw_json(value: serde_json::Value) -> Result<ResponseBody, BaseError> {
    let body =
        serde_json::to_string(&value).map_err(|e| BaseError::JsonSerializeFailed(e.to_string()))?;
    ResponseBody::raw(body, "application/json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn malformed_callbacks_never_return_success_ack() -> anyhow::Result<()> {
        let pool = Arc::new(sqlx::MySqlPool::connect_lazy(
            "mysql://unused:unused@127.0.0.1:1/unused_test",
        )?);
        let context = crate::addon::feishu::build_context(
            pool,
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?;
        let tools = Arc::new(yang_base::tools::ToolsBuilder::new().build()?);
        let key = crypto::derive_key("test-key");
        let invalid_json = crypto::encrypt_bytes(b"not-json", &key)?;
        let mut failures = Vec::new();
        for (body, encrypt_key, server_error) in [
            (serde_json::json!({"encrypt":"invalid"}), None, true),
            (
                serde_json::json!({"encrypt":"invalid"}),
                Some("test-key".into()),
                true,
            ),
            (
                serde_json::json!({"encrypt":invalid_json}),
                Some("test-key".into()),
                false,
            ),
            (
                serde_json::json!({"token":"valid", "type":"url_verification"}),
                None,
                false,
            ),
            (
                serde_json::json!({"token":"valid", "type":"unexpected", "challenge":"nonce"}),
                None,
                false,
            ),
        ] {
            let ctx = ActionContext::new(
                yang_base::action::Request::new(serde_json::json!({})),
                tools.clone(),
            );
            let result = handle(
                ctx,
                ChallengeRequest(body.clone()),
                context.as_ref().clone(),
                "valid".into(),
                encrypt_key,
            )
            .await;
            let correct = match result {
                Err(BaseError::ConfigError(_)) => server_error,
                Err(BaseError::ValidationFailed(_, _)) => !server_error,
                _ => false,
            };
            if !correct {
                failures.push(body);
            }
        }
        assert!(
            failures.is_empty(),
            "无效回调不得返回成功 ACK，且应区分服务端故障与客户端格式错误：{failures:?}"
        );
        Ok(())
    }

    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("日志锁失败"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn event_diagnostics_never_log_verification_tokens_or_decrypted_payloads(
    ) -> anyhow::Result<()> {
        const SECRET: &str = "test-verification-secret-never-log";
        let pool = Arc::new(sqlx::MySqlPool::connect_lazy(
            "mysql://unused:unused@127.0.0.1:1/unused_test",
        )?);
        let context = crate::addon::feishu::build_context(
            pool,
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?;
        let tools = Arc::new(yang_base::tools::ToolsBuilder::new().build()?);
        let ctx = || {
            ActionContext::new(
                yang_base::action::Request::new(serde_json::json!({})),
                tools.clone(),
            )
        };
        let logs = Logs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let key = crypto::derive_key("test-encrypt-key");
        for body in [
            serde_json::json!({"token": SECRET}),
            serde_json::json!({"token": SECRET, "type":"unexpected", "challenge":"nonce"}),
            serde_json::json!({"encrypt": crypto::encrypt_json(&serde_json::json!({"token": SECRET,"type":"url_verification","challenge":"nonce"}), &key)?}),
        ] {
            let encrypted = body.get("encrypt").is_some();
            let _ = handle(
                ctx(),
                ChallengeRequest(body),
                context.as_ref().clone(),
                SECRET.into(),
                encrypted.then(|| "test-encrypt-key".into()),
            )
            .await;
        }
        let invalid =
            crypto::encrypt_bytes(format!("{{\"token\":\"{SECRET}\",invalid").as_bytes(), &key)?;
        let valid = crypto::encrypt_json(
            &serde_json::json!({"token": SECRET,"type":"event_callback"}),
            &key,
        )?;
        let _ = super::super::receive_approval_event::handle(
            ctx(),
            super::super::receive_approval_event::EventRequest(
                serde_json::json!({"encrypt": valid}),
            ),
            context.as_ref().clone(),
            Some("different-token".into()),
            Some("test-encrypt-key".into()),
        )
        .await;
        let _ = super::super::receive_approval_event::handle(
            ctx(),
            super::super::receive_approval_event::EventRequest(
                serde_json::json!({"encrypt":invalid}),
            ),
            context.as_ref().clone(),
            Some(SECRET.into()),
            Some("test-encrypt-key".into()),
        )
        .await;
        let bytes = logs
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("日志锁失败"))?
            .clone();
        let output = String::from_utf8(bytes)?;
        assert!(output.contains("飞书"), "必须实际捕获诊断日志");
        assert!(!output.contains(SECRET), "诊断泄露认证材料");
        Ok(())
    }
}
