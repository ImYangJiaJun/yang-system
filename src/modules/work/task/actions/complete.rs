//! 受上限保护、租户原子的批量完成 Action。

use super::super::repository;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ParamInput, Params};
use yang_base::table::Record;
use yang_base::{Action, BaseError};

const MAX_BULK_TASKS: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteTasksInput {
    selected: Vec<Record>,
}

impl ParamInput for CompleteTasksInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct CompleteTasksOutput {
    requested: usize,
    affected: u64,
}

#[derive(Debug, Action)]
#[action(
    name = "complete",
    display_name = "批量完成",
    description = "一次将最多 100 个当前工作区任务标记为完成",
    method = "POST",
    path = "/api/v1/work/tasks/complete",
    permissions("work.task:write")
)]
pub(in crate::modules::work::task) struct CompleteTasksAction;

#[async_trait]
impl ActionHandler for CompleteTasksAction {
    type Input = CompleteTasksInput;
    type Output = CompleteTasksOutput;

    async fn index(
        &self,
        context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let ids = selected_ids(&input.selected)?;
        let values = ids
            .iter()
            .map(|id| serde_json::json!(id))
            .collect::<Vec<_>>();
        // tenant-boundary: transaction work-task-complete-transaction
        let mut transaction = context.begin_transaction().await?;
        if let Err(error) =
            repository::lock_tasks_for_completion(&context, &ids, &mut transaction).await
        {
            transaction
                .rollback()
                .await
                .map_err(BaseError::DatabaseTransactionFailed)?;
            return Err(error);
        }
        let affected = context
            .table_query()?
            .where_in("id", values)?
            .update_in_tx(&mut transaction, Record::new().set("status", "done"))
            .await?;
        transaction
            .commit()
            .await
            .map_err(BaseError::DatabaseTransactionFailed)?;
        Ok(CompleteTasksOutput {
            requested: ids.len(),
            affected,
        })
    }
}

fn selected_ids(selected: &[Record]) -> Result<Vec<i64>, BaseError> {
    if selected.is_empty() || selected.len() > MAX_BULK_TASKS {
        return Err(BaseError::ParamInvalid(
            "selected".to_string(),
            format!("必须选择 1..={MAX_BULK_TASKS} 个任务"),
        ));
    }
    let mut ids = BTreeSet::new();
    for record in selected {
        let id = record.require::<i64>("id")?;
        if id <= 0 {
            return Err(BaseError::ParamInvalid(
                "selected.id".to_string(),
                "必须是正整数".to_string(),
            ));
        }
        if !ids.insert(id) {
            return Err(BaseError::ParamInvalid(
                "selected".to_string(),
                "不得包含重复任务".to_string(),
            ));
        }
    }
    Ok(ids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(count: usize) -> Vec<Record> {
        (1..=count)
            .map(|id| Record::new().set("id", i64::try_from(id).unwrap_or_default()))
            .collect()
    }

    #[test]
    fn bulk_boundary_accepts_one_hundred_and_rejects_abuse() {
        assert_eq!(
            selected_ids(&selected(MAX_BULK_TASKS))
                .unwrap_or_else(|error| panic!("100 个任务应有效: {error}"))
                .len(),
            MAX_BULK_TASKS
        );
        assert!(selected_ids(&[]).is_err());
        assert!(selected_ids(&selected(MAX_BULK_TASKS + 1)).is_err());
        assert!(selected_ids(&[Record::new().set("id", 7), Record::new().set("id", 7)]).is_err());
        assert!(selected_ids(&[Record::new().set("id", 0)]).is_err());
        assert!(selected_ids(&[Record::new().set("title", "缺少 ID")]).is_err());
    }
}
