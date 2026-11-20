//! 拉取当前用户的飞书待办，并按审批定义分组。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::api;
use super::super::domain::computed::computed_fields;
use super::super::domain::view::{self, BatchUserView};
use super::access_token_or_error;
use crate::addon::feishu::domain::context::FeishuContext;

/// 列表请求的总预算：飞书出站单次 20s、PULL_RETRY 重试 3 次能到 61s，
/// 超过服务端 60s 请求超时后前端只会看到「操作结果尚未确认」。
/// 这里提前收敛成一条明确的超时响应。
const LIST_BUDGET: Duration = Duration::from_secs(25);

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListTasksInput {}

impl ParamInput for ListTasksInput {
    fn params() -> Params {
        Params::new()
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能访问自己的飞书身份与批量审批数据
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_list_tasks"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(HttpMethod::Get, "/api/v1/feishu/batch-approval/tasks")
        .display_name("批量审批待办")
        .description("列出当前用户的飞书待办并按审批定义分组")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    _input: ListTasksInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let token = match access_token_or_error(&ctx, &context, user_id).await {
        Ok(token) => token,
        Err(response) => return Ok(response),
    };
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书集成未配置"));
    };
    let list =
        match tokio::time::timeout(LIST_BUDGET, api::list_pending_tasks(&ctx, settings, &token))
            .await
        {
            Ok(Ok(list)) => list,
            Ok(Err(error)) => return Err(error.to_base()),
            Err(_) => {
                tracing::warn!(
                    budget_secs = LIST_BUDGET.as_secs(),
                    "拉取飞书待办列表超出预算"
                );
                return Ok(ApiResponse::fail(50401, "飞书待办列表响应超时，请稍后重试"));
            }
        };
    let mut definitions: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for task in &list.tasks {
        let code = task
            .definition_code
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let name = task
            .definition_name
            .clone()
            .unwrap_or_else(|| "未命名审批".to_string());
        definitions
            .entry(code)
            .and_modify(|entry| {
                entry.1 += 1;
                if entry.0.is_empty() {
                    entry.0 = name.clone();
                }
            })
            .or_insert((name, 1));
    }
    let definitions: Vec<serde_json::Value> = definitions
        .into_iter()
        .map(|(code, (name, count))| {
            serde_json::json!({ "code": code, "name": name, "count": count })
        })
        .collect();
    let approval_codes: Vec<String> = list
        .tasks
        .iter()
        .filter_map(|task| task.definition_code.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let views = view::load_many(&context, user_id, &approval_codes).await?;
    let view_payload = view_map(&approval_codes, &views);
    // 待办可能有上百条：逐条串行拉实例详情会把请求拖过 60 秒的 HTTP 超时。
    // 先按实例去重再并发预取，单条失败仍按「无详情」降级。
    let instance_codes: Vec<String> = list
        .tasks
        .iter()
        .map(|task| task.instance_code.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let details = api::instance_details(&ctx, settings, &instance_codes).await;
    let mut rows = Vec::with_capacity(list.tasks.len());
    for task in list.tasks {
        let task_id = task.task_id.clone();
        let view = task
            .definition_code
            .as_ref()
            .and_then(|code| views.get(code));
        let mut row = serde_json::to_value(&task)
            .map_err(|error| BaseError::Unknown(format!("序列化审批任务失败：{error}")))?;
        let detail = details.get(&task.instance_code).cloned().flatten();
        enrich_row(&mut row, detail.as_ref(), &task_id, view);
        rows.push(row);
    }
    ApiResponse::success(
        serde_json::json!({
            "tasks": rows,
            "definitions": definitions,
            "views": view_payload,
            "truncated": list.truncated,
        }),
        "ok",
    )
}

/// 把实例详情里的展示字段并入待办行；没有详情时保持任务列表的原始形状。
fn enrich_row(
    row: &mut Value,
    detail: Option<&Value>,
    task_id: &str,
    view: Option<&BatchUserView>,
) {
    let Value::Object(map) = row else {
        return;
    };
    let Some(detail) = detail else {
        return;
    };
    let nodes = current_node_names(detail, task_id);
    map.insert(
        "serial_number".to_string(),
        detail.get("serial_number").cloned().unwrap_or(Value::Null),
    );
    map.insert("form_fields".to_string(), Value::Array(form_fields(detail)));
    map.insert(
        "current_node_names".to_string(),
        Value::Array(nodes.iter().cloned().map(Value::String).collect()),
    );
    map.insert(
        "my_node_name".to_string(),
        my_node_name(detail, task_id)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if let Some(view) = view {
        let rules: Vec<_> = view
            .computed_fields
            .iter()
            .filter(|rule| rule.enabled)
            .cloned()
            .collect();
        map.insert(
            "computed_fields".to_string(),
            Value::Array(computed_fields(detail, &rules)),
        );
    }
}

fn view_map(
    approval_codes: &[String],
    views: &BTreeMap<String, BatchUserView>,
) -> BTreeMap<String, Value> {
    let empty = json!({ "computed_fields": [], "columns": [] });
    approval_codes
        .iter()
        .map(|code| {
            (
                code.clone(),
                views
                    .get(code)
                    .map(BatchUserView::to_json)
                    .unwrap_or_else(|| empty.clone()),
            )
        })
        .collect()
}

fn form_fields(detail: &Value) -> Vec<Value> {
    let rows = form_rows(detail);
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let name = ["name", "label", "title", "custom_id"]
                .iter()
                .find_map(|key| row.get(*key).and_then(Value::as_str))
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("字段 {}", index + 1));
            let value = row
                .get("value")
                .or_else(|| row.get("field_value"))
                .cloned()
                .unwrap_or_else(|| row.clone());
            let field_type = row.get("type").and_then(Value::as_str).unwrap_or("");
            let ext = row.get("ext").cloned().unwrap_or(Value::Null);
            let field_id = row.get("id").and_then(Value::as_str).unwrap_or("");
            if is_detail_type(field_type) {
                let rows = detail_rows(&value);
                return serde_json::json!({
                    "id": field_id,
                    "name": name,
                    "value": Value::Null,
                    "field_type": field_type,
                    "ext": ext,
                    "row_count": rows.len(),
                    "preview": preview_cells(&rows),
                });
            }
            serde_json::json!({
                "id": field_id,
                "name": name,
                "value": value,
                "field_type": field_type,
                "ext": ext,
            })
        })
        .collect()
}

fn form_rows(detail: &Value) -> Vec<Value> {
    match detail.get("form") {
        Some(Value::Array(rows)) => rows.clone(),
        Some(Value::String(raw)) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn is_detail_type(field_type: &str) -> bool {
    field_type.eq_ignore_ascii_case("fieldList") || field_type.eq_ignore_ascii_case("field_list")
}

/// 明细的 `value` 是行数组，每行又是单元格数组；也兼容被序列化成字符串的形态。
fn detail_rows(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(rows) => rows.clone(),
        Value::String(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 第一行前 3 个非空单元格的文本，仅用于列表里的折叠预览。
fn preview_cells(rows: &[Value]) -> Vec<String> {
    rows.first()
        .and_then(Value::as_array)
        .map(|cells| {
            cells
                .iter()
                .map(|cell| text_of(cell.get("value").unwrap_or(cell)))
                .filter(|text| !text.is_empty())
                .take(3)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn text_of(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string(),
        Value::Array(items) => items
            .iter()
            .map(text_of)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("、"),
    }
}

fn task_list(detail: &Value) -> &[Value] {
    detail
        .get("task_list")
        .or_else(|| detail.get("tasks"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn task_node_name(task: &Value) -> Option<&str> {
    task.get("node_name")
        .or_else(|| task.get("name"))
        .and_then(Value::as_str)
}

fn task_is_pending(task: &Value) -> bool {
    match task.get("status") {
        Some(Value::String(status)) => status.eq_ignore_ascii_case("PENDING") || status == "1",
        Some(Value::Number(status)) => status.as_i64() == Some(1),
        _ => false,
    }
}

fn current_node_names(detail: &Value, task_id: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(nodes) = detail.get("current_nodes").and_then(Value::as_array) {
        for node in nodes {
            if let Some(name) = ["name", "node_name", "node_id"]
                .iter()
                .find_map(|key| node.get(*key).and_then(Value::as_str))
            {
                push_name(&mut names, name);
            }
        }
    }
    for task in task_list(detail) {
        if !task_is_pending(task) {
            continue;
        }
        if let Some(name) = task_node_name(task) {
            push_name(&mut names, name);
        }
    }
    if names.is_empty() {
        if let Some(task) = task_list(detail)
            .iter()
            .find(|task| task_identity(task) == Some(task_id))
        {
            if let Some(name) = task_node_name(task) {
                push_name(&mut names, name);
            }
        }
    }
    names
}

fn push_name(names: &mut Vec<String>, name: &str) {
    if !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}

fn task_identity(task: &Value) -> Option<&str> {
    task.get("id")
        .or_else(|| task.get("task_id"))
        .and_then(Value::as_str)
}

fn my_node_name(detail: &Value, task_id: &str) -> Option<String> {
    task_list(detail)
        .iter()
        .find(|task| task_identity(task) == Some(task_id))
        .and_then(task_node_name)
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::{current_node_names, enrich_row, form_fields, my_node_name, view_map};
    use serde_json::{json, Value};

    #[test]
    fn form_fields_parse_json_string_and_field_value() {
        let detail = json!({
            "form": "[{\"id\":\"w1\",\"name\":\"金额\",\"value\":{\"text\":\"100\"}},{\"label\":\"部门\",\"field_value\":\"研发\"},{\"name\":\"附件\",\"type\":\"attachmentV2\",\"ext\":\"'发票.pdf'\",\"value\":[\"https://files.example.com/a.pdf\"]}]"
        });
        let fields = form_fields(&detail);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0]["id"], "w1");
        assert_eq!(fields[0]["name"], "金额");
        assert_eq!(fields[0]["value"]["text"], "100");
        assert_eq!(fields[1]["name"], "部门");
        assert_eq!(fields[1]["value"], "研发");
        assert_eq!(fields[0]["field_type"], "");
        assert_eq!(fields[0]["ext"], serde_json::Value::Null);
        assert_eq!(fields[2]["field_type"], "attachmentV2");
        assert_eq!(fields[2]["ext"], "'发票.pdf'");
        assert_eq!(fields[2]["value"][0], "https://files.example.com/a.pdf");
    }

    #[test]
    fn form_fields_condense_detail_rows() {
        let detail = json!({
            "form": [
                {
                    "id": "items",
                    "name": "费用明细",
                    "type": "fieldList",
                    "value": [
                        [
                            {"id": "name", "type": "input", "value": "笔记本"},
                            {"id": "qty", "type": "number", "value": 2},
                            {"id": "note", "type": "input", "value": ""},
                            {"id": "price", "type": "amount", "value": {"text": "4500"}}
                        ],
                        [{"id": "name", "type": "input", "value": "鼠标"}]
                    ]
                },
                {"id": "total", "name": "合计", "type": "amount", "value": 9000}
            ]
        });
        let fields = form_fields(&detail);
        assert_eq!(fields[0]["value"], serde_json::Value::Null);
        assert_eq!(fields[0]["row_count"], 2);
        assert_eq!(fields[0]["preview"], json!(["笔记本", "2", "4500"]));
        assert_eq!(fields[1]["value"], 9000);
        assert!(fields[1].get("row_count").is_none());
    }

    #[test]
    fn view_map_returns_empty_views_for_unconfigured_codes() {
        let views = std::collections::BTreeMap::new();
        let codes = vec!["A".to_string(), "B".to_string()];
        let payload = view_map(&codes, &views);
        assert_eq!(payload["A"]["computed_fields"], json!([]));
        assert_eq!(payload["B"]["columns"], json!([]));
    }

    #[test]
    fn current_nodes_keep_order_and_mark_my_node() {
        let detail = json!({
            "task_list": [
                {"id": "t1", "node_name": "部门审批", "status": "PENDING"},
                {"id": "t2", "node_name": "财务审批", "status": 1}
            ]
        });
        assert_eq!(
            current_node_names(&detail, "t1"),
            vec!["部门审批", "财务审批"]
        );
        assert_eq!(my_node_name(&detail, "t2").as_deref(), Some("财务审批"));
    }

    #[test]
    fn enrich_row_copies_serial_number_from_instance_detail() {
        let detail = json!({
            "serial_number": "202611200024",
            "form": []
        });
        let mut row = json!({ "task_id": "t1" });
        enrich_row(&mut row, Some(&detail), "t1", None);
        assert_eq!(row["serial_number"], json!("202611200024"));
    }

    #[test]
    fn enrich_row_keeps_null_serial_number_when_detail_lacks_it() {
        let detail = json!({ "form": [] });
        let mut row = json!({ "task_id": "t1" });
        enrich_row(&mut row, Some(&detail), "t1", None);
        assert_eq!(row["serial_number"], Value::Null);
    }

    #[test]
    fn enrich_row_leaves_list_shape_when_detail_is_unavailable() {
        let mut row = json!({ "task_id": "t1" });
        enrich_row(&mut row, None, "t1", None);
        assert!(row.get("serial_number").is_none());
        assert!(row.get("form_fields").is_none());
    }
}
