//! 拉取当前用户的飞书待办，并按审批定义分组。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

use super::super::api;
use super::access_token_or_error;
use crate::addon::feishu::domain::context::FeishuContext;

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
    let list = api::list_pending_tasks(&ctx, settings, &token)
        .await
        .map_err(|error| error.to_base())?;
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
    let mut rows = Vec::with_capacity(list.tasks.len());
    let mut detail_cache: BTreeMap<String, Option<Value>> = BTreeMap::new();
    for task in list.tasks {
        let task_id = task.task_id.clone();
        let mut row = serde_json::to_value(&task)
            .map_err(|error| BaseError::Unknown(format!("序列化审批任务失败：{error}")))?;
        if let Value::Object(map) = &mut row {
            let detail = if let Some(cached) = detail_cache.get(&task.instance_code) {
                cached.clone()
            } else {
                // 详情接口只接受 tenant token；单条失败不应让整个待办列表失败。
                let fetched = api::instance_detail(&ctx, settings, &task.instance_code)
                    .await
                    .ok();
                detail_cache.insert(task.instance_code.clone(), fetched.clone());
                fetched
            };
            if let Some(detail) = detail {
                let nodes = current_node_names(&detail, &task_id);
                map.insert(
                    "form_fields".to_string(),
                    Value::Array(form_fields(&detail)),
                );
                map.insert(
                    "current_node_names".to_string(),
                    Value::Array(nodes.iter().cloned().map(Value::String).collect()),
                );
                map.insert(
                    "my_node_name".to_string(),
                    my_node_name(&detail, &task_id)
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
            }
        }
        rows.push(row);
    }
    ApiResponse::success(
        serde_json::json!({
            "tasks": rows,
            "definitions": definitions,
            "truncated": list.truncated,
        }),
        "ok",
    )
}

fn form_fields(detail: &Value) -> Vec<Value> {
    let rows = match detail.get("form") {
        Some(Value::Array(rows)) => rows.clone(),
        Some(Value::String(raw)) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
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
            serde_json::json!({
                "name": name,
                "value": value,
                "field_type": field_type,
                "ext": ext,
            })
        })
        .collect()
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
    use super::{current_node_names, form_fields, my_node_name};
    use serde_json::json;

    #[test]
    fn form_fields_parse_json_string_and_field_value() {
        let detail = json!({
            "form": "[{\"name\":\"金额\",\"value\":{\"text\":\"100\"}},{\"label\":\"部门\",\"field_value\":\"研发\"}]"
        });
        let fields = form_fields(&detail);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0]["name"], "金额");
        assert_eq!(fields[0]["value"]["text"], "100");
        assert_eq!(fields[1]["name"], "部门");
        assert_eq!(fields[1]["value"], "研发");
        assert_eq!(fields[0]["field_type"], "");
        assert_eq!(fields[0]["ext"], serde_json::Value::Null);
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
}
