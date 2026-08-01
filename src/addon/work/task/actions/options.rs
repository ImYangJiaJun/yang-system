//! 父任务关系选择 Action。

use async_trait::async_trait;
use yang_base::action::builtin::RelationOptionsAction;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::table::{RelationOptionsRequest, RelationOptionsResponse};
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "options",
    display_name = "父任务选择器",
    description = "按当前个人工作区分页搜索父任务",
    method = "POST",
    path = "/api/v1/work/tasks/options",
    permissions("work.task:read")
)]
pub(in crate::addon::work::task) struct TaskOptionsAction {
    inner: RelationOptionsAction,
}

impl TaskOptionsAction {
    pub(in crate::addon::work::task) fn new() -> Result<Self, BaseError> {
        Ok(Self {
            inner: RelationOptionsAction::new("id", ["title"])?,
        })
    }
}

#[async_trait]
impl ActionHandler for TaskOptionsAction {
    type Input = RelationOptionsRequest;
    type Output = RelationOptionsResponse;

    async fn index(
        &self,
        context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        self.inner.index(context, input).await
    }
}
