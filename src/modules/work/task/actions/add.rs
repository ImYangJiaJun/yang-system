//! 新增任务前校验项目与父任务关系。

use super::super::repository;
use async_trait::async_trait;
use yang_base::action::builtin::InsertResult;
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::table::Record;
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "add",
    display_name = "新增任务",
    description = "在个人工作区中新增关系一致的任务"
)]
pub(in crate::modules::work::task) struct AddTaskAction;

#[async_trait]
impl TypedHandler for AddTaskAction {
    type Input = Record;
    type Output = InsertResult;

    async fn handle(
        &self,
        context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let project_id = input.require("project_project")?;
        let parent_id = input.optional("parent_task")?;
        repository::validate_task_links(&context, project_id, parent_id, None).await?;
        let (affected, id) = context.table_query()?.insert_returning_id(input).await?;
        Ok(InsertResult { affected, id })
    }
}
