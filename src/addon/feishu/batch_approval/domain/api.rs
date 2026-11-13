//! 飞书审批 API 的最小客户端。
//!
//! 列表与任务动作使用用户自己的 `user_access_token`；实例详情按官方要求改用
//! `tenant_access_token`（用户 token 会报 99991668）。
//! 返回值保留为 `serde_json::Value`，因为详情/表单/时间线的字段形状以真机为准，
//! 过早强类型化只会在飞书字段调整时制造 502。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;

use yang_base::action::ActionContext;
use yang_base::BaseError;

use crate::config::FeishuSettings;

use crate::addon::feishu::domain::outbound::{
    send_with_retry, OutboundMethod, OutboundRequest, PULL_RETRY,
};
use crate::addon::feishu::domain::outbound::{HttpClientTransport, TokioSleeper};
use crate::addon::feishu::domain::outbound_setup;
use crate::addon::feishu::domain::tenant_token::{
    FeishuCredentials, RedisTenantTokenCache, TenantTokenProvider, FEISHU_OPEN_BASE,
};

const MAX_PAGES: usize = 10;
const PAGE_SIZE: usize = 100;
const REQUEST_TIMEOUT_SECS: u64 = 20;
/// 实例详情的并发上限。一个待办上百条时逐条串行会直接撞上 HTTP 请求超时；
/// 取 8 既能把总耗时压到个位数秒，又不会让飞书侧限流（默认连接池上限 10）。
const DETAIL_CONCURRENCY: usize = 8;
/// 实例详情预取的总预算。列表里详情只是列表页的富化信息，超预算就整批降级成
/// 「无详情」，不能让它把整页待办拖成超时。
const DETAIL_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

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

fn dedupe_tasks(tasks: &mut Vec<TaskItem>) {
    let mut seen = HashSet::new();
    tasks.retain(|task| seen.insert((task.task_id.clone(), task.instance_code.clone())));
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
            dedupe_tasks(&mut tasks);
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
    dedupe_tasks(&mut tasks);
    Ok(TaskList {
        tasks,
        truncated: true,
    })
}

async fn tenant_access_token(
    ctx: &ActionContext,
    settings: &FeishuSettings,
) -> Result<String, ApiError> {
    let deployment = ctx
        .tools()
        .extension::<crate::authorization::AuthorizationVersionCache>()
        .map_err(|error| ApiError {
            code: 0,
            message: format!("读取部署命名空间失败：{error}"),
        })?
        .deployment()
        .to_string();
    let cache = Arc::new(RedisTenantTokenCache::new(
        ctx.tools()
            .cache()
            .map_err(|error| ApiError {
                code: 0,
                message: format!("读取 Redis 失败：{error}"),
            })?
            .clone(),
        &deployment,
    ));
    let transport = Arc::new(HttpClientTransport::new(
        ctx.tools()
            .http()
            .map_err(|error| ApiError {
                code: 0,
                message: format!("读取 HTTP 客户端失败：{error}"),
            })?
            .clone(),
    ));
    let tokens = TenantTokenProvider::new(
        cache,
        transport,
        Arc::new(TokioSleeper),
        FeishuCredentials {
            app_id: settings.app_id.clone().unwrap_or_default(),
            app_secret: settings.app_secret.clone().unwrap_or_default(),
        },
        &deployment,
    )
    .map_err(|error| ApiError {
        code: 0,
        message: format!("构建 tenant_access_token provider 失败：{error}"),
    })?;
    tokens
        .tenant_access_token()
        .await
        .map_err(|error| ApiError {
            code: 0,
            message: format!("获取 tenant_access_token 失败：{error}"),
        })
}

pub(crate) async fn instance_detail(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    instance_code: &str,
) -> Result<Value, ApiError> {
    let access_token = tenant_access_token(ctx, settings).await?;
    instance_detail_with_token(ctx, settings, &access_token, instance_code).await
}

/// 批量取实例详情：tenant token 只换一次，按唯一实例去重后以有界并发拉取。
///
/// 单条失败不影响其他行（与列表页原有「拿不到详情就降级」的语义一致）。
pub(crate) async fn instance_details(
    ctx: &ActionContext,
    settings: &FeishuSettings,
    instance_codes: &[String],
) -> BTreeMap<String, Option<Value>> {
    if instance_codes.is_empty() {
        return BTreeMap::new();
    }
    let access_token = match tenant_access_token(ctx, settings).await {
        Ok(token) => Arc::new(token),
        Err(error) => {
            tracing::warn!(
                error = %error.message,
                count = instance_codes.len(),
                "批量获取审批实例详情失败，本轮全部按无详情处理"
            );
            return instance_codes
                .iter()
                .map(|code| (code.clone(), None))
                .collect();
        }
    };

    let started = std::time::Instant::now();
    let mut details = BTreeMap::new();
    let mut remaining = instance_codes.iter();
    let mut set = JoinSet::new();
    for _ in 0..DETAIL_CONCURRENCY {
        let Some(code) = remaining.next() else { break };
        spawn_instance_detail(&mut set, ctx, settings, &access_token, code);
    }
    // 飞书偶发变慢时，单条详情的 20s×3 次重试就能吃满整个 HTTP 超时。给预取
    // 留一个总预算：超时后剩余的按「无详情」降级，列表本身照常返回。
    let finished = tokio::time::timeout(DETAIL_BUDGET, async {
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((code, detail)) => {
                    details.insert(code, detail);
                }
                Err(error) => {
                    // JoinSet 的 JoinError 不携带实例 Code，只能记录任务已丢失；
                    // 对应行会按无详情降级，不影响整个列表。
                    tracing::warn!(error = %error, "审批实例详情任务异常退出");
                }
            }
            if let Some(code) = remaining.next() {
                spawn_instance_detail(&mut set, ctx, settings, &access_token, code);
            }
        }
    })
    .await;
    if finished.is_err() {
        tracing::warn!(
            count = instance_codes.len(),
            collected = details.len(),
            budget_secs = DETAIL_BUDGET.as_secs(),
            "审批实例详情预取超出预算，剩余按无详情降级"
        );
    }
    tracing::debug!(
        count = instance_codes.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "批量获取审批实例详情完成"
    );
    details
}

fn spawn_instance_detail(
    set: &mut JoinSet<(String, Option<Value>)>,
    ctx: &ActionContext,
    settings: &FeishuSettings,
    access_token: &Arc<String>,
    instance_code: &str,
) {
    let ctx = ctx.clone();
    let settings = settings.clone();
    let access_token = Arc::clone(access_token);
    let instance_code = instance_code.to_string();
    set.spawn(async move {
        let detail = match instance_detail_with_token(
            &ctx,
            &settings,
            &access_token,
            &instance_code,
        )
        .await
        {
            Ok(detail) => Some(detail),
            Err(error) => {
                tracing::debug!(
                    instance_code = %instance_code,
                    error = %error.message,
                    "审批实例详情获取失败，按无详情降级"
                );
                None
            }
        };
        (instance_code, detail)
    });
}

async fn instance_detail_with_token(
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
    open_id: &str,
) -> Result<Value, ApiError> {
    // 通讯录详情用 tenant token：用户 token 对 contact 接口可能返回空 data，
    // 会被上层误判成「目标用户不存在」；tenant token 还天然限定在本租户内。
    let access_token = tenant_access_token(ctx, settings).await?;
    call(
        ctx,
        settings,
        &access_token,
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
