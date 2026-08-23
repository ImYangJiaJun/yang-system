//! 新增任务前校验项目与父任务关系。

use super::super::domain::repository;
use yang_base::action::builtin::InsertResult;
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::BaseError;

pub(super) async fn handle(ctx: ActionContext, input: Record) -> Result<InsertResult, BaseError> {
    let project_id = input.require("project_project")?;
    let parent_id = input.optional("parent_task")?;
    // tenant-boundary: transaction work-task-add-transaction
    let mut transaction = ctx.begin_transaction().await?;
    let result = async {
        repository::lock_workspace(&ctx, &mut transaction).await?;
        repository::validate_task_links_in_tx(&ctx, &mut transaction, project_id, parent_id, None)
            .await?;
        let (affected, id) = ctx
            .table_query()?
            .insert_returning_id_in_tx(&mut transaction, input)
            .await?;
        Ok(InsertResult { affected, id })
    }
    .await;
    repository::finish_transaction(transaction, result).await
}
