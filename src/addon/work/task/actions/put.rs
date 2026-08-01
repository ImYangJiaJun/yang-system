//! 修改任务前校验项目、父任务与树环。

use super::super::repository;
use async_trait::async_trait;
use serde_json::Value;
use yang_base::action::builtin::{AffectedResult, PutInput};
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "put",
    display_name = "修改任务",
    description = "修改任务并拒绝跨工作区、跨项目父子关系与树环"
)]
pub(in crate::addon::work::task) struct PutTaskAction;

#[async_trait]
impl TypedHandler for PutTaskAction {
    type Input = PutInput;
    type Output = AffectedResult;

    async fn handle(
        &self,
        context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        if input.data.as_map().is_empty() {
            return Err(BaseError::ParamInvalid(
                "data".to_string(),
                "至少需要一个字段".to_string(),
            ));
        }
        let task_id = positive_id("id", &input.id)?;
        // tenant-boundary: transaction work-task-put-transaction
        let mut transaction = context.begin_transaction().await?;
        let result = async {
            repository::lock_workspace(&context, &mut transaction).await?;
            let current =
                repository::current_links_in_tx(&context, &mut transaction, task_id).await?;
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
                &context,
                &mut transaction,
                project_id,
                parent_id,
                Some(task_id),
            )
            .await?;
            let affected = context
                .table_query()?
                .where_primary_key_eq(input.id)?
                .update_in_tx(&mut transaction, input.data)
                .await?;
            Ok(AffectedResult { affected })
        }
        .await;
        repository::finish_transaction(transaction, result).await
    }
}

fn positive_id(name: &str, value: &Value) -> Result<i64, BaseError> {
    value
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| BaseError::ParamInvalid(name.to_string(), "必须是正整数".to_string()))
}
