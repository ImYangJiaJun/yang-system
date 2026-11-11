//! 单个审批实例详情（只读）。

use std::sync::Arc;

use serde_json::Value;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
use yang_base::BaseError;

use super::super::api;
use super::access_token_or_error;
use crate::addon::feishu::domain::context::FeishuContext;
use crate::addon::feishu::sso::credentials;

yang_base::params! {
    pub(super) TaskDetailInput {
        #[param(source = path)]
        task_id: Str::new().require(true),
        #[param(source = query)]
        instance_code: Str::new().require(true),
    }
}

pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    // auth: authenticated-only 自服务操作——登录用户只能访问自己的飞书身份与批量审批数据
    module
        .action_fn(
            yang_base::action_name!("feishu_batch_task_detail"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Get,
            "/api/v1/feishu/batch-approval/tasks/{task_id}",
        )
        .display_name("审批任务详情")
        .description("按 instance_code 拉取审批实例详情（表单/时间线/当前节点）")
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: TaskDetailInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    let user_id = crate::addon::feishu::sso::require_current_user(&ctx)?;
    let token = match access_token_or_error(&ctx, &context, user_id).await {
        Ok(token) => token,
        Err(response) => return Ok(response),
    };
    let Some(credential) = credentials::load_credential(&ctx, &context, user_id).await? else {
        return Ok(ApiResponse::fail(40901, "尚未绑定飞书身份"));
    };
    let Some(settings) = context.settings() else {
        return Ok(ApiResponse::fail(50301, "飞书集成未配置"));
    };
    let detail = api::instance_detail(&ctx, settings, &token, &input.instance_code)
        .await
        .map_err(|error| error.to_base())?;
    match detail_access(&detail, &input.task_id, &credential.open_id) {
        DetailAccess::Allowed => {}
        DetailAccess::TaskNotInInstance => {
            return Ok(ApiResponse::fail(40402, "任务不属于该审批实例"));
        }
        DetailAccess::NotParticipant => {
            return Ok(ApiResponse::fail(40301, "无权查看该审批实例"));
        }
    }
    ApiResponse::success(detail, "ok")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAccess {
    Allowed,
    NotParticipant,
    TaskNotInInstance,
}

/// 纵深防御：用户 token 之外，再用详情里的 `open_id` 确认当前用户确实参与该实例。
///
/// 详情可能因飞书返回字段差异没有 `tasks`；此时仍按操作记录/发起人判断参与关系，
/// 不把「字段缺失」误判成越权。`tasks` 存在时必须能找到同一个 `task_id`，否则这个
/// task 与 `instance_code` 不是一对，不能让前端把它当成该实例的待办。
fn detail_access(detail: &Value, task_id: &str, open_id: &str) -> DetailAccess {
    let tasks = detail.get("tasks").and_then(Value::as_array);
    if let Some(tasks) = tasks {
        if !tasks.iter().any(|task| {
            task.get("task_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == task_id)
        }) {
            return DetailAccess::TaskNotInInstance;
        }
    }
    if participant_open_ids(detail).any(|value| value == open_id) {
        DetailAccess::Allowed
    } else {
        DetailAccess::NotParticipant
    }
}

fn participant_open_ids(detail: &Value) -> impl Iterator<Item = &str> {
    let mut values = Vec::new();
    if let Some(open_id) = detail
        .get("initiator")
        .and_then(|value| value.get("open_id"))
        .and_then(Value::as_str)
    {
        values.push(open_id);
    }
    for key in ["tasks", "operation_records"] {
        if let Some(records) = detail.get(key).and_then(Value::as_array) {
            for record in records {
                if let Some(open_id) = record.get("open_id").and_then(Value::as_str) {
                    values.push(open_id);
                }
                // 当前节点审批人在 user_id_type=open_id 下也用 `user_id` 字段返回。
                if let Some(approvers) = record.get("approvers").and_then(Value::as_array) {
                    for approver in approvers {
                        if let Some(open_id) = approver.get("user_id").and_then(Value::as_str) {
                            values.push(open_id);
                        }
                    }
                }
            }
        }
    }
    values.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn participant_must_be_in_tasks_or_operation_records() {
        let detail = json!({
            "tasks": [{"task_id": "t1", "open_id": "ou_me"}],
            "operation_records": [{"open_id": "ou_other"}],
        });
        assert_eq!(detail_access(&detail, "t1", "ou_me"), DetailAccess::Allowed);
        assert_eq!(
            detail_access(&detail, "t1", "ou_other"),
            DetailAccess::Allowed
        );
        assert_eq!(
            detail_access(&detail, "t1", "ou_stranger"),
            DetailAccess::NotParticipant
        );
    }

    #[test]
    fn task_must_belong_to_the_instance() {
        let detail = json!({
            "tasks": [{"task_id": "t1", "open_id": "ou_me"}],
        });
        assert_eq!(
            detail_access(&detail, "t2", "ou_me"),
            DetailAccess::TaskNotInInstance
        );
    }

    #[test]
    fn missing_tasks_still_allows_a_participant() {
        let detail = json!({
            "initiator": {"open_id": "ou_me"},
            "current_nodes": [{"approvers": [{"user_id": "ou_me"}]}],
        });
        assert_eq!(detail_access(&detail, "t1", "ou_me"), DetailAccess::Allowed);
        assert_eq!(
            detail_access(&detail, "t1", "ou_stranger"),
            DetailAccess::NotParticipant
        );
    }
}
