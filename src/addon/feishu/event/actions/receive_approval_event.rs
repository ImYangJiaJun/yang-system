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
use crate::addon::feishu::domain::token::tokens_equal;
use crate::config::FeishuSettings;

/// 事件接收请求体——用 `Value` 接收任意 JSON，避免反序列化失败。
///
/// 飞书推送的加密请求是 `{"encrypt":"..."}`，明文请求是完整事件 JSON。
/// 两种格式都由本 handler 处理。
#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct EventRequest(pub(super) Value);

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
    // 没有 verification_token 就不注册：否则公开端点无从校验来源，等于接受任意伪造事件。
    let Some(verification_token) = settings.event_verification_token.clone() else {
        return module;
    };
    let encrypt_key = settings.event_encrypt_key.clone();
    module
        .action_fn(
            yang_base::action_name!("receive_approval_event"),
            move |ctx, input| {
                handle(
                    ctx,
                    input,
                    context.clone(),
                    Some(verification_token.clone()),
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
/// 可见性 `pub(super)`：challenge 端点（同属 `event::actions` 树）在检测到
/// 非 challenge 请求时也会委托给本函数；架构门禁要求 Action 恰好是
/// `pub(super) async fn handle` 形态，不能再放宽。
pub(super) async fn handle(
    ctx: ActionContext,
    input: EventRequest,
    context: FeishuContext,
    verification_token: Option<String>,
    encrypt_key: Option<String>,
) -> Result<ResponseBody, BaseError> {
    let raw_body = &input.0;
    tracing::info!(
        body_size = raw_body.to_string().len(),
        "=== 飞书审批事件到达 ==="
    );

    // 解密或直接用明文。
    //
    // 失败一律返回 `Err`（框架映射 5xx）而不是 200 + error body：HTTP 200 是
    // 飞书判定推送成功的**唯一**信号（响应体它不看），返回 200 会让这条事件
    // 永久丢失；5xx 触发飞书按 15s/5min/1h/6h 重推（最多 4 次、约 7.1 小时
    // 窗口），这是解密类故障唯一的自愈机会。
    let mut event_body = match raw_body.get("encrypt").and_then(Value::as_str) {
        Some(encrypted) => {
            tracing::debug!("检测到加密事件，开始解密");
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
                        tracing::debug!(decrypted_size = decrypted.len(), "飞书事件解密成功");
                        parsed
                    }
                    Err(e) => {
                        tracing::error!(
                            category = ?e.classify(),
                            line = e.line(),
                            column = e.column(),
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
            // 配置了 Encrypt Key 后，飞书所有推送都应加密；明文只可能是伪造或配置漂移。
            if encrypt_key.is_some() {
                return Err(BaseError::Unauthorized(
                    "已配置 event_encrypt_key，拒绝明文事件".to_string(),
                ));
            }
            tracing::debug!("明文事件，无需解密");
            raw_body.clone()
        }
    };

    // v2.0 事件：验签材料在 `header.token`，事件身份在 `header.event_id`。
    // 必须在 v1 的顶层 `token` 校验之前分流，否则 v2 恒定 401。
    if is_v2_event(&event_body) {
        return handle_v2(ctx, event_body, context, verification_token.as_deref()).await;
    }

    // 验签 fail-closed：缺 Token 或不匹配一律拒绝；比较走常数时间。
    let Some(expected_token) = verification_token.as_deref() else {
        return Err(BaseError::ConfigError(
            "飞书事件 verification_token 未配置".to_string(),
        ));
    };
    let Some(actual_token) = event_body.get("token").and_then(Value::as_str) else {
        return Err(BaseError::Unauthorized(
            "飞书事件缺少 verification_token".to_string(),
        ));
    };
    if !tokens_equal(actual_token, expected_token) {
        return Err(BaseError::Unauthorized(
            "飞书事件 verification_token 不匹配".to_string(),
        ));
    }
    tracing::debug!("飞书事件 verification_token 校验通过");
    // 认证材料只参与本次校验，不进入诊断报文、数据库和备份。
    if let Some(object) = event_body.as_object_mut() {
        object.remove("token");
    }

    // 提取事件字段（飞书推送结构：顶层有 type/event_callback/uuid/token，
    // 业务字段在 event 嵌套层里）
    //
    // 事件 id 取**顶层** `uuid`。注意：`approval_instance` 事件的 `event.uuid`
    // 是**实例 uuid**（创建实例时传入的那个），与事件 id 同名不同义——
    // 不要"顺手"改成 `event.uuid`，那会静默破坏幂等去重。
    let event = event_body.get("event").and_then(Value::as_object);
    let event_id = event_body.get("uuid").and_then(Value::as_str).unwrap_or("");
    let event_type = event
        .and_then(|e| e.get("type").and_then(Value::as_str))
        .unwrap_or("unknown");
    let instance_code = event
        .and_then(|e| e.get("instance_code").and_then(Value::as_str))
        .unwrap_or("");
    // 不替缺失身份造空键或推断 UUID；同一空键会把不同事件误判为重复投递。
    if event_body.get("type").and_then(Value::as_str) != Some("event_callback")
        || !matches!(event_type, "approval_instance" | "approval_task")
        || event_id.trim().is_empty()
        || event_id.chars().count() > 128
        || instance_code.trim().is_empty()
        || instance_code.chars().count() > 128
    {
        return Err(BaseError::ValidationFailed(
            "event".into(),
            "事件必须包含受支持的类型、非空顶层 uuid 与 instance_code（均最多 128 字符）".into(),
        ));
    }
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

    tracing::debug!(
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

    tracing::debug!("开始插入事件落地表");

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
        Err(error) => {
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

// ---------------------------------------------------------------------------
// v2.0 事件（多维表格记录变更）
// ---------------------------------------------------------------------------

/// 只处理的 v2 事件类型；其余事件验签后 200 忽略。
pub(super) const BITABLE_RECORD_CHANGED_EVENT: &str = "drive.file.bitable_record_changed_v1";

/// 是否 v2.0 事件：顶层 `schema == "2.0"` 或存在带 `event_id` 的 `header`。
///
/// challenge 请求没有 `header`，不会被误判；控制台只有一个回调 URL，v2 事件
/// 可能落到 `/event/challenge`，因此两端点共用本判据。
pub(super) fn is_v2_event(body: &Value) -> bool {
    body.get("schema").and_then(Value::as_str) == Some("2.0")
        || body
            .get("header")
            .and_then(Value::as_object)
            .is_some_and(|header| header.get("event_id").is_some())
}

/// 解析后的 v2 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedV2Event {
    pub(super) event_id: String,
    pub(super) event_type: String,
    pub(super) file_token: String,
    pub(super) table_id: String,
    pub(super) record_id: String,
    pub(super) action: String,
    pub(super) changed_field_ids: Option<String>,
    /// 已删除 `header.token` / 顶层 `token` 的报文。
    pub(super) redacted_payload: String,
}

/// 校验 v2 事件并提取落表字段。
///
/// **认证材料在序列化前删除**：`raw_payload` 会进数据库与备份，绝不能带 token。
/// 返回 `Ok(None)` 表示事件类型不处理（调用方回 200 让飞书停止重推）。
pub(super) fn parse_v2_event(
    body: &mut Value,
    expected_token: Option<&str>,
) -> Result<Option<ParsedV2Event>, BaseError> {
    let Some(expected_token) = expected_token else {
        return Err(BaseError::Unauthorized(
            "飞书事件 verification_token 未配置".to_string(),
        ));
    };
    let header = body
        .get_mut("header")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            BaseError::ValidationFailed("header".into(), "v2.0 事件缺少 header".into())
        })?;
    let actual_token = header
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !tokens_equal(actual_token, expected_token) {
        return Err(BaseError::Unauthorized(
            "飞书 v2.0 事件 header.token 不匹配".to_string(),
        ));
    }
    // 先把 token 从报文里摘掉，后面任何序列化都不会带上它。
    header.remove("token");

    let event_id = header
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let event_type = header
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if event_id.is_empty()
        || event_id.chars().count() > 128
        || event_type.is_empty()
        || event_type.chars().count() > 64
    {
        return Err(BaseError::ValidationFailed(
            "header".into(),
            "v2.0 事件必须包含非空 header.event_id（≤128）与 header.event_type（≤64）".into(),
        ));
    }

    // v1 遗留的顶层 token 一并删除（防止旧格式混入）。
    if let Some(object) = body.as_object_mut() {
        object.remove("token");
    }

    if event_type != BITABLE_RECORD_CHANGED_EVENT {
        return Ok(None);
    }

    let event = body
        .get("event")
        .and_then(Value::as_object)
        .ok_or_else(|| BaseError::ValidationFailed("event".into(), "v2.0 事件缺少 event".into()))?;
    let file_token = event
        .get("file_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let table_id = event
        .get("table_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let record_id = event
        .get("record_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let action = event
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if file_token.is_empty()
        || file_token.chars().count() > 128
        || table_id.is_empty()
        || table_id.chars().count() > 64
        || record_id.is_empty()
        || record_id.chars().count() > 64
        || action.is_empty()
        || action.chars().count() > 32
    {
        return Err(BaseError::ValidationFailed(
            "event".into(),
            "多维表格事件必须包含 file_token（≤128）、table_id（≤64）、record_id（≤64）与 action（≤32）"
                .into(),
        ));
    }

    let changed_field_ids = extract_changed_field_ids(event);
    let redacted_payload = body.to_string();
    Ok(Some(ParsedV2Event {
        event_id,
        event_type,
        file_token,
        table_id,
        record_id,
        action,
        changed_field_ids,
        redacted_payload,
    }))
}

/// 变更字段 id 列表：兼容 `changed_fields` / `changed_field_ids` / `field_ids`
/// 三种上游键，元素可以是字符串或 `{field_id}` / `{id}` 对象。
///
/// 缺键返回 `None`（不是空数组）：worker 据此区分「上游没给」与「给了空列表」，
/// 前者不能用「没有相关字段」去过滤。
fn extract_changed_field_ids(event: &serde_json::Map<String, Value>) -> Option<String> {
    for key in ["changed_field_ids", "changed_fields", "field_ids"] {
        let Some(value) = event.get(key) else {
            continue;
        };
        let items = match value {
            Value::Array(items) => items,
            Value::String(text) => {
                let text = text.trim();
                if text.is_empty() {
                    return Some("[]".to_string());
                }
                // 某些租户把列表直接序列化成 JSON 字符串。
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    return extract_changed_field_ids_from_value(&parsed);
                }
                return Some(serde_json::json!([text]).to_string());
            }
            _ => return None,
        };
        let mut ids = Vec::new();
        for item in items {
            let id = match item {
                Value::String(text) => text.trim().to_string(),
                Value::Object(object) => object
                    .get("field_id")
                    .or_else(|| object.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                _ => String::new(),
            };
            if !id.is_empty() {
                ids.push(id);
            }
        }
        return Some(serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string()));
    }
    None
}

fn extract_changed_field_ids_from_value(value: &Value) -> Option<String> {
    match value {
        Value::Array(_) => {
            let mut object = serde_json::Map::new();
            object.insert("changed_field_ids".to_string(), value.clone());
            extract_changed_field_ids(&object)
        }
        _ => None,
    }
}

/// 处理 v2.0 事件：验签 → 落缓冲表 → 200。
async fn handle_v2(
    _ctx: ActionContext,
    mut body: Value,
    context: FeishuContext,
    verification_token: Option<&str>,
) -> Result<ResponseBody, BaseError> {
    let Some(parsed) = parse_v2_event(&mut body, verification_token)? else {
        tracing::debug!("v2.0 事件类型不在评论关注范围内，已忽略");
        return raw_json(serde_json::json!({"success": true, "ignored": true}));
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut record = yang_base::table::Record::new();
    record.insert("event_id", serde_json::json!(&parsed.event_id));
    record.insert("event_type", serde_json::json!(&parsed.event_type));
    record.insert("file_token", serde_json::json!(&parsed.file_token));
    record.insert("table_id", serde_json::json!(&parsed.table_id));
    record.insert("record_id", serde_json::json!(&parsed.record_id));
    record.insert("action", serde_json::json!(&parsed.action));
    record.insert(
        "changed_field_ids",
        parsed
            .changed_field_ids
            .as_deref()
            .map_or(Value::Null, |value| serde_json::json!(value)),
    );
    record.insert("raw_payload", serde_json::json!(&parsed.redacted_payload));
    record.insert("received_at", serde_json::json!(now));
    record.insert("processed_at", Value::Null);

    let result = context.bitable_events().query().insert(record).await;
    match result {
        Ok(_) => {
            tracing::info!(
                event_id = %parsed.event_id,
                action = %parsed.action,
                table_id = %parsed.table_id,
                "多维表格记录变更事件已落地，等待评论 worker 处理"
            );
            raw_json(serde_json::json!({"success": true}))
        }
        Err(error) => {
            let rendered = error.to_string();
            if crate::db::is_unique_violation(&error)
                || rendered.contains("Duplicate")
                || rendered.contains("1062")
            {
                tracing::info!(
                    event_id = %parsed.event_id,
                    "多维表格事件重复投递（event_id 唯一索引冲突），已忽略"
                );
                return raw_json(serde_json::json!({"success": true, "duplicate": true}));
            }
            tracing::error!(
                error = %rendered,
                event_id = %parsed.event_id,
                "多维表格事件落地失败，返回 5xx 等待飞书重推"
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn missing_event_identity_is_rejected_before_database_access() -> anyhow::Result<()> {
        let pool = Arc::new(sqlx::MySqlPool::connect_lazy(
            "mysql://unused:unused@127.0.0.1:1/unused_test",
        )?);
        let context = crate::addon::feishu::build_context(
            pool,
            Arc::new(crate::config::SecuritySettings::default()),
            None,
        )?;
        let tools = Arc::new(yang_base::tools::ToolsBuilder::new().build()?);
        let valid = serde_json::json!({"token":"valid", "type":"event_callback", "uuid":"event-one", "event":{"type":"approval_instance", "instance_code":"instance-one"}});
        for (field, value) in [
            ("uuid", Value::Null),
            ("uuid", Value::String(" ".into())),
            ("uuid", Value::String("x".repeat(129))),
            ("type", Value::String("unknown".into())),
            ("event.type", Value::String("unknown".into())),
            ("event.instance_code", Value::Null),
        ] {
            let mut body = valid.clone();
            if let Some(nested) = field.strip_prefix("event.") {
                body["event"][nested] = value;
            } else {
                body[field] = value;
            }
            let ctx = ActionContext::new(
                yang_base::action::Request::new(serde_json::json!({})),
                tools.clone(),
            );
            let error = handle(
                ctx,
                EventRequest(body),
                context.as_ref().clone(),
                Some("valid".into()),
                None,
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("缺少事件身份必须拒绝"));
            assert!(
                matches!(error, BaseError::ValidationFailed(_, _)),
                "必须在数据库访问前验证 {field}，实际：{error:?}"
            );
        }
        Ok(())
    }

    // ---- v2.0 多维表格事件 ----

    fn v2_body(event_type: &str) -> Value {
        serde_json::json!({
            "schema": "2.0",
            "header": {
                "event_id": "evt-1",
                "token": "secret",
                "event_type": event_type
            },
            "event": {
                "file_token": "bascnTest",
                "table_id": "tblTest",
                "record_id": "recTest",
                "action": "record_edited",
                "changed_fields": ["fldA", {"field_id": "fldB"}]
            }
        })
    }

    #[test]
    fn v2_detection_covers_schema_and_header_but_not_v1_or_challenge() {
        assert!(is_v2_event(&v2_body(BITABLE_RECORD_CHANGED_EVENT)));
        let mut header_only = v2_body(BITABLE_RECORD_CHANGED_EVENT);
        header_only
            .as_object_mut()
            .unwrap_or_else(|| panic!("对象"))
            .remove("schema");
        assert!(is_v2_event(&header_only));
        assert!(!is_v2_event(
            &serde_json::json!({"token":"t","type":"event_callback","event":{}})
        ));
        assert!(!is_v2_event(
            &serde_json::json!({"token":"t","type":"url_verification","challenge":"c"})
        ));
    }

    #[test]
    fn v2_event_parses_identity_and_redacts_the_header_token() {
        let mut body = v2_body(BITABLE_RECORD_CHANGED_EVENT);
        let parsed = parse_v2_event(&mut body, Some("secret"))
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("应解析出事件"));
        assert_eq!(parsed.event_id, "evt-1");
        assert_eq!(parsed.event_type, BITABLE_RECORD_CHANGED_EVENT);
        assert_eq!(parsed.file_token, "bascnTest");
        assert_eq!(parsed.table_id, "tblTest");
        assert_eq!(parsed.record_id, "recTest");
        assert_eq!(parsed.action, "record_edited");
        assert_eq!(
            parsed.changed_field_ids.as_deref(),
            Some(r#"["fldA","fldB"]"#)
        );
        assert!(
            !parsed.redacted_payload.contains("secret"),
            "落库报文不得包含 header.token: {}",
            parsed.redacted_payload
        );
        assert!(
            body["header"].get("token").is_none(),
            "解析后必须把 header.token 从报文里摘掉"
        );
    }

    #[test]
    fn v2_event_rejects_missing_or_mismatched_tokens() {
        let mut missing = v2_body(BITABLE_RECORD_CHANGED_EVENT);
        missing["header"]
            .as_object_mut()
            .unwrap_or_else(|| panic!("对象"))
            .remove("token");
        assert!(matches!(
            parse_v2_event(&mut missing, Some("secret")),
            Err(BaseError::Unauthorized(_))
        ));

        let mut wrong = v2_body(BITABLE_RECORD_CHANGED_EVENT);
        assert!(matches!(
            parse_v2_event(&mut wrong, Some("other")),
            Err(BaseError::Unauthorized(_))
        ));
        // 未配置 token 时 fail-closed。
        let mut unconfigured = v2_body(BITABLE_RECORD_CHANGED_EVENT);
        assert!(matches!(
            parse_v2_event(&mut unconfigured, None),
            Err(BaseError::Unauthorized(_))
        ));
    }

    #[test]
    fn v2_event_requires_event_id_and_ignores_other_types_after_verification() {
        let mut missing_id = v2_body(BITABLE_RECORD_CHANGED_EVENT);
        missing_id["header"]["event_id"] = Value::Null;
        assert!(matches!(
            parse_v2_event(&mut missing_id, Some("secret")),
            Err(BaseError::ValidationFailed(_, _))
        ));

        let mut other = v2_body("drive.file.deleted_v1");
        let parsed =
            parse_v2_event(&mut other, Some("secret")).unwrap_or_else(|error| panic!("{error}"));
        assert!(parsed.is_none(), "未关注的事件类型应 200 忽略");
        assert!(
            other["header"].get("token").is_none(),
            "忽略分支也必须先删除 token"
        );
    }

    #[test]
    fn duplicate_deliveries_carry_the_same_event_identity() {
        // 幂等由 feishu_bitable_event.event_id 唯一索引承担；这里钉住解析出的
        // 身份稳定，避免上游重推被解析成不同 id 而重复入队。
        let first = parse_v2_event(&mut v2_body(BITABLE_RECORD_CHANGED_EVENT), Some("secret"))
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("应解析"));
        let second = parse_v2_event(&mut v2_body(BITABLE_RECORD_CHANGED_EVENT), Some("secret"))
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("应解析"));
        assert_eq!(first, second);
    }

    #[test]
    fn v2_event_requires_the_bitable_coordinates() {
        for (path, value) in [
            ("event.file_token", Value::Null),
            ("event.table_id", Value::String(" ".into())),
            ("event.record_id", Value::Null),
            ("event.action", Value::Null),
        ] {
            let mut body = v2_body(BITABLE_RECORD_CHANGED_EVENT);
            body["event"][path.strip_prefix("event.").unwrap_or_default()] = value;
            assert!(
                matches!(
                    parse_v2_event(&mut body, Some("secret")),
                    Err(BaseError::ValidationFailed(_, _))
                ),
                "{path} 缺失必须拒绝"
            );
        }
    }
}
