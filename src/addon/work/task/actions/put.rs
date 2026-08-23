//! 修改任务前校验项目、父任务与树环。

use super::super::domain::repository;
use serde_json::Value;
use yang_base::action::builtin::{AffectedResult, PutInput};
use yang_base::action::ActionContext;
use yang_base::BaseError;

pub(super) async fn handle(
    ctx: ActionContext,
    input: PutInput,
) -> Result<AffectedResult, BaseError> {
    if input.data.as_map().is_empty() {
        return Err(BaseError::ParamInvalid(
            "data".to_string(),
            "至少需要一个字段".to_string(),
        ));
    }
    let task_id = positive_id("id", &input.id)?;
    // tenant-boundary: transaction work-task-put-transaction
    let mut transaction = ctx.begin_transaction().await?;
    let result = async {
        repository::lock_workspace(&ctx, &mut transaction).await?;
        let current = repository::current_links_in_tx(&ctx, &mut transaction, task_id).await?;
        let project_id = match input.data.get("project_project") {
            Some(value) => positive_id("project_project", value)?,
            None => current.project_id,
        };
        let parent_id = match input.data.get("parent_task") {
            Some(Value::Null) => None,
            Some(value) => Some(positive_id("parent_task", value)?),
            None => current.parent_id,
        };
        repository::validate_task_links_in_tx(
            &ctx,
            &mut transaction,
            project_id,
            parent_id,
            Some(task_id),
        )
        .await?;
        let affected = ctx
            .table_query()?
            .where_primary_key_eq(input.id)?
            .update_in_tx(&mut transaction, input.data)
            .await?;
        Ok(AffectedResult { affected })
    }
    .await;
    repository::finish_transaction(transaction, result).await
}

fn positive_id(name: &str, value: &Value) -> Result<i64, BaseError> {
    value
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| BaseError::ParamInvalid(name.to_string(), "必须是正整数".to_string()))
}
