//! 飞书审批用户态 API 的最小客户端。
//!
//! 所有调用都使用用户自己的 `user_access_token`；服务端不持有 tenant 审批身份。
//! 返回值保留为 `serde_json::Value`，因为详情/表单/时间线的字段形状以真机为准，
//! 过早强类型化只会在飞书字段调整时制造 502。

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use yang_base::action::ActionContext;
use yang_base::BaseError;

use crate::config::FeishuSettings;

use crate::addon::feishu::domain::outbound::{
    send_with_retry, OutboundMethod, OutboundRequest, PULL_RETRY,
};
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::tenant_token::FEISHU_OPEN_BASE;

const MAX_PAGES: usize = 10;
const PAGE_SIZE: usize = 100;
const REQUEST_TIMEOUT_SECS: u64 = 20;

/// 实例 Code 作为路径段：只保留 RFC3986 unreserved 字符，其余按字节百分号编码。
fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn value_as_string(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn string_from_value<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    value_as_string(value).ok_or_else(|| de::Error::custom("期望字符串或数字"))
}

fn optional_string_from_value<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(value_as_string))
}

fn optional_bool_from_value<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::Bool(value)) => Some(value),
        Some(Value::Number(value)) => value.as_i64().map(|value| value != 0),
        Some(Value::String(value)) => match value.as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) | None => None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskItem {
    #[serde(deserialize_with = "string_from_value")]
    pub(crate) task_id: String,
    #[serde(deserialize_with = "string_from_value")]
    pub(crate) instance_code: String,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) definition_code: Option<String>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) definition_name: Option<String>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) title: Option<String>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) initiator_name: Option<String>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) instance_status: Option<String>,
    /// `/tasks` 的任务分组：1=待办，2=已办，17/18=通知。
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) topic: Option<String>,
    #[serde(default)]
    pub(crate) summaries: Option<Value>,
    #[serde(default, deserialize_with = "optional_bool_from_value")]
    pub(crate) support_api_operate: Option<bool>,
    #[serde(default)]
    pub(crate) link: Option<Value>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) instance_external_id: Option<String>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) task_external_id: Option<String>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    pub(crate) status: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskList {
    pub(crate) tasks: Vec<TaskItem>,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ApiError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn to_base(&self) -> BaseError {
        BaseError::Unknown(format!(
            "飞书审批 API 失败（code={}）：{}",
            self.code, self.message
        ))
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    code: i32,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TasksPage {
    #[serde(default)]
    tasks: Vec<TaskItem>,
    #[serde(default, deserialize_with = "optional_bool_from_value")]
    has_more: Option<bool>,
    #[serde(default, deserialize_with = "optional_string_from_value")]
    page_token: Option<String>,
}

async fn call(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &str,
    method: OutboundMethod,
    path: &str,
    query: Vec<(String, String)>,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    let outbound = outbound_setup::build(ctx, settings).map_err(|error| ApiError {
        code: 0,
        message: format!("构造飞书出站传输失败：{error}"),
    })?;
    let request = OutboundRequest {
        method,
        url: format!("{FEISHU_OPEN_BASE}{path}"),
        query,
        bearer_token: Some(access_token.to_string()),
        json_body: body,
        raw_body: None,
        binary_limit: None,
        timeout_secs: Some(REQUEST_TIMEOUT_SECS),
        idempotent: matches!(method, OutboundMethod::Get),
    };
    let response = if request.idempotent {
        send_with_retry(
            outbound.transport(),
            outbound.sleeper(),
            &request,
            PULL_RETRY,
        )
        .await
        .map_err(|failure| ApiError {
            code: 0,
            message: failure.message,
        })?
    } else {
        // 写请求不走自动重试：只有明确的限流才允许上层退避重放同一 item。
        match outbound.transport().send(request.clone()).await {
            Ok(response) if response.status == 429 => {
                return Err(ApiError {
                    code: 429,
                    message: "飞书接口限流".to_string(),
                });
            }
            Ok(response) if response.status >= 500 => {
                return Err(ApiError {
                    code: 0,
                    message: format!("飞书服务异常：HTTP {}", response.status),
                });
            }
            Ok(response) => response,
            Err(BaseError::RateLimitExceeded { .. }) => {
                return Err(ApiError {
                    code: 429,
                    message: "飞书接口限流".to_string(),
                });
            }
            Err(error) => {
                return Err(ApiError {
                    code: 0,
                    message: error.to_string(),
                });
            }
        }
    };
    let envelope: Envelope = serde_json::from_str(&response.body).map_err(|error| ApiError {
        code: if response.status >= 400 {
            i32::from(response.status)
        } else {
            0
        },
        message: format!("飞书响应解析失败：{error}"),
    })?;
    if envelope.code != 0 {
        return Err(ApiError {
            code: envelope.code,
            message: envelope.msg.unwrap_or_else(|| "飞书返回失败".to_string()),
        });
    }
    Ok(envelope.data.unwrap_or(Value::Null))
}

pub(crate) async fn list_pending_tasks(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &str,
) -> Result<TaskList, ApiError> {
    let mut tasks = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![
            ("topic".to_string(), "1".to_string()),
            ("page_size".to_string(), PAGE_SIZE.to_string()),
            ("user_id_type".to_string(), "open_id".to_string()),
        ];
        if let Some(token) = &page_token {
            query.push(("page_token".to_string(), token.clone()));
        }
        let data = call(
            ctx,
            settings,
            access_token,
            OutboundMethod::Get,
            "/open-apis/approval/v4/tasks",
            query,
            None,
        )
        .await?;
        let page: TasksPage = serde_json::from_value(data).map_err(|error| ApiError {
            code: 0,
            message: format!("任务列表响应解析失败：{error}"),
        })?;
        tasks.extend(page.tasks);
        if !page.has_more.unwrap_or(false) {
            return Ok(TaskList {
                tasks,
                truncated: false,
            });
        }
        page_token = page.page_token;
        if page_token.is_none() {
            break;
        }
    }
    Ok(TaskList {
        tasks,
        truncated: true,
    })
}

pub(crate) async fn instance_detail(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &str,
    instance_code: &str,
) -> Result<Value, ApiError> {
    let path = format!(
        "/open-apis/approval/v4/instances/{}",
        encode_path_segment(instance_code)
    );
    call(
        ctx,
        settings,
        access_token,
        OutboundMethod::Get,
        &path,
        vec![("user_id_type".to_string(), "open_id".to_string())],
        None,
    )
    .await
}

pub(crate) async fn search_users(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &str,
    query: &str,
) -> Result<Value, ApiError> {
    call(
        ctx,
        settings,
        access_token,
        OutboundMethod::Get,
        "/open-apis/search/v1/user",
        vec![
            ("query".to_string(), query.to_string()),
            ("page_size".to_string(), "20".to_string()),
        ],
        None,
    )
    .await
}

pub(crate) async fn user_detail(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &str,
    open_id: &str,
) -> Result<Value, ApiError> {
    call(
        ctx,
        settings,
        access_token,
        OutboundMethod::Get,
        &format!("/open-apis/contact/v3/users/{open_id}"),
        vec![("user_id_type".to_string(), "open_id".to_string())],
        None,
    )
    .await
}

pub(crate) async fn task_action(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &str,
    action: &str,
    body: Value,
) -> Result<Value, ApiError> {
    if !matches!(
        action,
        "pass" | "refuse" | "forward" | "rollback" | "add_sign"
    ) {
        return Err(ApiError {
            code: 0,
            message: format!("不支持的审批动作：{action}"),
        });
    }
    call(
        ctx,
        settings,
        access_token,
        OutboundMethod::Post,
        &format!("/open-apis/approval/v4/tasks/{action}"),
        Vec::new(),
        Some(body),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{TaskItem, TasksPage};

    #[test]
    fn task_item_accepts_numeric_ids_and_status() {
        let item: TaskItem = serde_json::from_value(serde_json::json!({
            "task_id": 123456789,
            "instance_code": "INSTANCE-1",
            "status": 1,
            "support_api_operate": 1
        }))
        .unwrap_or_else(|error| panic!("numeric 形态应能解析: {error}"));

        assert_eq!(item.task_id, "123456789");
        assert_eq!(item.instance_code, "INSTANCE-1");
        assert_eq!(item.status.as_deref(), Some("1"));
        assert_eq!(item.support_api_operate, Some(true));
    }

    #[test]
    fn tasks_page_ignores_null_optional_fields() {
        let page: TasksPage = serde_json::from_value(serde_json::json!({
            "tasks": [{
                "task_id": "1",
                "instance_code": "INSTANCE-1",
                "definition_code": null,
                "title": null
            }],
            "has_more": false
        }))
        .unwrap_or_else(|error| panic!("空字段应能解析: {error}"));

        assert_eq!(page.tasks.len(), 1);
        assert_eq!(page.tasks[0].definition_code, None);
        assert_eq!(page.tasks[0].title, None);
    }
}
