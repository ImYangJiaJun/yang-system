//! 人工重试表单反向同步（前端控制台）。
//!
//! 只做「把 `review` / `retry` 重置回 `retry`」；真正的映射与写入由 worker 在
//! 持租约时执行，绝不在这里绕过哈希、守卫或字段白名单。

use std::sync::Arc;

use serde_json::json;
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::definition::{HttpMethod, Key, ModuleSpec};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::infrastructure::audit;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) RetryFormSyncInput {
        #[param(source = path)]
        task_id: Key::new()
            .title("任务 ID")
            .require(true),
    }
}

/// 注册人工重试端点。
pub(super) fn register(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    module
        .action_fn(
            yang_base::action_name!("retry_form_sync"),
            move |ctx, input| handle(ctx, input, Arc::clone(&context)),
        )
        .route(
            HttpMethod::Post,
            "/api/v1/feishu/approval/tasks/{task_id}/form-sync/retry",
        )
        .display_name("重试表单反向同步")
        .description("把待人工核查或重试中的表单同步任务重新排入队列，由 worker 执行")
        .permissions(["feishu.approval.write"])
        .register()
}

pub(super) async fn handle(
    ctx: ActionContext,
    input: RetryFormSyncInput,
    context: Arc<FeishuContext>,
) -> Result<ApiResponse, BaseError> {
    if input.task_id <= 0 {
        return Err(BaseError::ParamInvalid(
            "task_id".to_string(),
            "任务 ID 必须是正整数".to_string(),
        ));
    }

    let row = context
        .approval_tasks()
        .query()
        .select_fields(&["id", "form_sync_state", "form_sync_attempts"])?
        .where_eq("id", json!(input.task_id))?
        .optional()
        .await?
        .ok_or_else(|| BaseError::RecordNotFound("审批任务不存在".to_string()))?;
    let previous_state: String = row
        .optional::<String>("form_sync_state")?
        .unwrap_or_else(|| "disabled".to_string());
    let previous_attempts: i64 = row.optional("form_sync_attempts")?.unwrap_or(0);
    if !matches!(previous_state.as_str(), "review" | "retry") {
        return Err(BaseError::ParamInvalid(
            "task_id".to_string(),
            "只有待人工核查或重试中的任务可以重试".to_string(),
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut transaction = ctx.begin_transaction().await?;
    let result: Result<serde_json::Value, BaseError> = async {
        let mut record = Record::new();
        record.insert("form_sync_state", json!("retry"));
        record.insert("form_sync_attempts", json!(0));
        record.insert("form_sync_next_at", json!(now));
        record.insert("form_sync_error", serde_json::Value::Null);
        let affected = context
            .approval_tasks()
            .query()
            .where_eq("id", json!(input.task_id))?
            .where_in("form_sync_state", vec![json!("review"), json!("retry")])?
            .update_in_tx(&mut transaction, record)
            .await?;
        if affected == 0 {
            return Err(BaseError::ParamInvalid(
                "task_id".to_string(),
                "任务同步状态已变化，请刷新后重试".to_string(),
            ));
        }
        let event = audit::succeeded_event(
            &ctx,
            None,
            None,
            audit::entity("feishu_approval_task", input.task_id)?,
            Some(audit::summary([
                ("form_sync_state", json!(previous_state)),
                ("form_sync_attempts", json!(previous_attempts)),
            ])?),
            Some(audit::summary([
                ("form_sync_state", json!("retry")),
                ("requeued", json!(true)),
            ])?),
        )?;
        audit::append_in_tx(&mut transaction, &event).await?;
        Ok(json!({
            "task_id": input.task_id,
            "requeued": true,
            "previous_state": previous_state,
        }))
    }
    .await;
    let value = FeishuContext::finish_transaction(transaction, result).await?;
    Ok(ApiResponse::success_value(value, "表单同步任务已重新排队"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ParamInput;

    #[test]
    fn input_contract_requires_path_task_id() {
        let params = <RetryFormSyncInput as ParamInput>::params();
        let id = params
            .as_slice()
            .iter()
            .find(|param| param.name.as_str() == "task_id")
            .unwrap_or_else(|| panic!("应声明 task_id 参数"));
        assert_eq!(id.source, yang_base::definition::ParamSource::Path);
        assert!(id.required);
    }

    #[tokio::test]
    async fn declares_write_permission_and_route() {
        let module = super::super::tests::registered_module(false);
        let action = module
            .actions()
            .iter()
            .find(|spec| spec.route.path.contains("/form-sync/retry"))
            .unwrap_or_else(|| panic!("表单同步重试路由必须注册"));
        assert_eq!(
            action.permissions,
            vec!["feishu.approval.write".to_string()]
        );
    }
}
