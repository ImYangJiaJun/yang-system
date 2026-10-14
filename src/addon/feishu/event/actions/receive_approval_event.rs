//! 飞书审批事件接收 Action。
//!
//! 接收飞书推送的审批事件（`approval_instance` / `approval_task`），
//! 验签解密后落事件落地表 `feishu_approval_event`，3 秒内返回 200。
//!
//! # store-then-process 架构
//!
//! 3 秒窗口内只做「验签 + 解密 + 落表」，绝不做飞书出站调用（富化 GET）。
//! 富化由后续的 worker 异步处理（`processed_at IS NULL` 即待处理队列）。

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::domain::crypto;
use crate::config::FeishuSettings;

/// 事件接收请求体——用 `Value` 接收任意 JSON，避免反序列化失败。
///
/// 飞书推送的加密请求是 `{"encrypt":"..."}`，明文请求是完整事件 JSON。
/// 两种格式都由本 handler 处理。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct EventRequest(pub(crate) Value);

impl ParamInput for EventRequest {
    fn params() -> Params {
        Params::new()
    }
}

/// 注册事件接收端点。
pub(super) fn register(
    module: ModuleSpec,
    context: FeishuContext,
    settings: &FeishuSettings,
) -> ModuleSpec {
    let verification_token = settings.event_verification_token.clone();
    let encrypt_key = settings.event_encrypt_key.clone();
    module
        .action_fn(
            yang_base::action_name!("receive_approval_event"),
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
        .route(HttpMethod::Post, "/api/v1/feishu/approval/event")
        .display_name("飞书审批事件接收")
        .description("接收飞书推送的审批状态变更事件，落事件落地表")
        .public()
        .register()
}

/// 处理飞书事件推送。
///
/// 流程：
/// 1. 验签（verification_token）
/// 2. 解密（如果有 encrypt 字段）
/// 3. 提取事件字段（event_id, event_type, instance_code, status, operate_time）
/// 4. 落事件落地表（幂等：event_id 唯一索引天然去重）
/// 5. 3 秒内返回 200
///
/// 可见性 `pub(crate)`：challenge 端点在检测到非 challenge 请求时也会委托给本函数。
pub(crate) async fn handle(
    ctx: ActionContext,
    input: EventRequest,
    context: FeishuContext,
    verification_token: Option<String>,
    encrypt_key: Option<String>,
) -> Result<ResponseBody, BaseError> {
    let raw_body = &input.0;
    tracing::info!(
        body = %raw_body,
        body_size = raw_body.to_string().len(),
        "=== 飞书审批事件到达 ==="
    );

    // 解密或直接用明文。
    //
    // 失败一律返回 `Err`（框架映射 5xx）而不是 200 + error body：HTTP 200 是
    // 飞书判定推送成功的**唯一**信号（响应体它不看），返回 200 会让这条事件
    // 永久丢失；5xx 触发飞书按 15s/5min/1h/6h 重推（最多 4 次、约 7.1 小时
    // 窗口），这是解密类故障唯一的自愈机会。
    let event_body = match raw_body.get("encrypt").and_then(Value::as_str) {
        Some(encrypted) => {
            tracing::info!("检测到加密事件，开始解密");
            let Some(key_raw) = encrypt_key.as_deref() else {
                tracing::error!("飞书发送了加密事件，但 event_encrypt_key 未配置");
                return Err(BaseError::ConfigError(
                    "飞书发送了加密事件，但 event_encrypt_key 未配置".into(),
                ));
            };
            let key = crypto::derive_key(key_raw);
            match crypto::decrypt_bytes(encrypted, &key) {
                Ok(decrypted) => match serde_json::from_slice::<Value>(&decrypted) {
                    Ok(parsed) => {
                        tracing::info!(
                            decrypted = %parsed,
                            decrypted_size = decrypted.len(),
                            "飞书事件解密成功"
                        );
                        parsed
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            decrypted_preview = %String::from_utf8_lossy(&decrypted[..decrypted.len().min(200)]),
                            "飞书事件解密后不是合法 JSON"
                        );
                        return Err(BaseError::ValidationFailed(
                            "decrypted_body".into(),
                            "飞书事件解密后不是合法 JSON".into(),
                        ));
                    }
                },
                Err(e) => {
                    tracing::error!(error = %e, "飞书事件解密失败");
                    return Err(BaseError::ConfigError(format!(
                        "飞书事件解密失败（Encrypt Key 不匹配或密文损坏）：{e}"
                    )));
                }
            }
        }
        None => {
            tracing::info!("明文事件，无需解密");
            raw_body.clone()
        }
    };

    // 验签（仅日志，不阻断）
    if let Some(expected_token) = verification_token.as_deref() {
        if let Some(actual_token) = event_body.get("token").and_then(Value::as_str) {
            if actual_token != expected_token {
                tracing::warn!(
                    feishu_token = %actual_token,
                    expected_token = %expected_token,
                    "飞书事件 verification_token 不匹配，仍处理事件"
                );
            } else {
                tracing::info!("verification_token 校验通过");
            }
        } else {
            tracing::warn!("事件中缺少 token 字段");
        }
    }

    // 提取事件字段（飞书推送结构：顶层有 type/event_callback/uuid/token，
    // 业务字段在 event 嵌套层里）
    //
    // 事件 id 取**顶层** `uuid`。注意：`approval_instance` 事件的 `event.uuid`
    // 是**实例 uuid**（创建实例时传入的那个），与事件 id 同名不同义——
    // 不要"顺手"改成 `event.uuid`，那会静默破坏幂等去重。
    let event = event_body.get("event").and_then(Value::as_object);
    let event_id = event_body
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or("");
    let event_type = event
        .and_then(|e| e.get("type").and_then(Value::as_str))
        .unwrap_or("unknown");
    let instance_code = event
        .and_then(|e| e.get("instance_code").and_then(Value::as_str))
        .unwrap_or("");
    let status = event
        .and_then(|e| e.get("status").and_then(Value::as_str))
        .unwrap_or("");
    // operate_time 官方字段表标注为毫秒级时间戳，但 REVERTED 等场景实际发的是
    // int64 而非字符串——两种形态都收，只认字符串会把撤销事件的时间静默丢成 NULL。
    let operate_time = event
        .and_then(|e| e.get("operate_time"))
        .and_then(|value| match value {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .unwrap_or_default();

    tracing::info!(
        event_id = %event_id,
        event_type = %event_type,
        instance_code = %instance_code,
        status = %status,
        operate_time = %operate_time,
        "提取的事件字段"
    );

    // 落事件落地表
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut record = yang_base::table::Record::new();
    record.insert("event_id", serde_json::json!(event_id));
    record.insert("event_type", serde_json::json!(event_type));
    record.insert("instance_code", serde_json::json!(instance_code));
    record.insert("status", serde_json::json!(status));
    record.insert("received_at", serde_json::json!(now));
    record.insert("processed_at", serde_json::Value::Null); // 显式设置为 NULL，确保 where_null 能查到
    if !operate_time.is_empty() {
        if let Ok(ts) = operate_time.parse::<i64>() {
            record.insert("operate_time", serde_json::json!(ts / 1000)); // 毫秒 → 秒
        }
    }
    record.insert("raw_payload", serde_json::json!(event_body.to_string()));

    tracing::info!("开始插入事件落地表");

    // 幂等插入：event_id 唯一索引，重复投递天然去重。
    //
    // **必须显式提交事务**（finish_transaction）：只 `insert_in_tx` 不 commit 的话，
    // `transaction` 离开作用域时回滚，事件永远进不了落地表——生产上表现为
    // 「已落地」日志与 worker 端 `raw_row_count: 0` 并存（2026-10-14 事故根因）。
    let mut transaction = ctx.begin_transaction().await?;
    let result = context
        .approval_events()
        .query()
        .insert_in_tx(&mut transaction, record)
        .await;

    match result {
        Ok(_) => {
            FeishuContext::finish_transaction(transaction, Ok(())).await?;
            tracing::info!(
                event_id = %event_id,
                event_type = %event_type,
                instance_code = %instance_code,
                status = %status,
                "=== 飞书事件已落地，等待 worker 处理 ==="
            );
            raw_json(serde_json::json!({"success": true}))
        }
        Err(e) => {
            let error = BaseError::from(e);
            let err_str = error.to_string();
            // 唯一索引冲突 = 重复投递：不算错误。回滚空事务后返回 200，
            // 让飞书停止重推（这正是重复投递该有的应答）。
            if err_str.contains("Duplicate") || err_str.contains("1062") {
                let _ = FeishuContext::finish_transaction::<()>(transaction, Err(error)).await;
                tracing::info!(
                    event_id = %event_id,
                    instance_code = %instance_code,
                    "飞书事件重复投递（event_id 唯一索引冲突），已忽略"
                );
                return raw_json(serde_json::json!({"success": true, "duplicate": true}));
            }
            // 真实落库失败：**不能返回 200**——HTTP 200 是飞书判定推送成功的
            // 唯一信号（响应体它不看），返回 200 会让这条事件永久丢失。回滚后
            // 把错误抛给框架（DbError 归因映射 500/503），飞书按
            // 15s/5min/1h/6h 重推，最多 4 次、约 7.1 小时窗口。
            tracing::error!(
                error = %err_str,
                event_id = %event_id,
                "飞书事件落地失败，返回 5xx 等待飞书重推"
            );
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::error!(error = %rollback_error, "飞书事件事务回滚失败");
            }
            Err(error)
        }
    }
}

/// 构造 raw JSON 响应（不套框架包络）。
fn raw_json(value: serde_json::Value) -> Result<ResponseBody, BaseError> {
    let body =
        serde_json::to_string(&value).map_err(|e| BaseError::JsonSerializeFailed(e.to_string()))?;
    ResponseBody::raw(body, "application/json")
}
