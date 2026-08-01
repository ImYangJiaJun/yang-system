//! 项目关系选择 Action。

use async_trait::async_trait;
use yang_base::action::builtin::RelationOptionsAction;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::table::{RelationOptionsRequest, RelationOptionsResponse};
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "options",
    display_name = "项目选择器",
    description = "按当前个人工作区分页搜索项目",
    method = "POST",
    path = "/api/v1/work/projects/options",
    permissions("work.project:read")
)]
pub(in crate::addon::work::project) struct ProjectOptionsAction {
    inner: RelationOptionsAction,
}

impl ProjectOptionsAction {
    pub(in crate::addon::work::project) fn new() -> Result<Self, BaseError> {
        Ok(Self {
            inner: RelationOptionsAction::new("id", ["name"])?,
        })
    }
}

#[async_trait]
impl ActionHandler for ProjectOptionsAction {
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
