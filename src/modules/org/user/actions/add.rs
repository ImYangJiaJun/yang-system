//! 新增企业成员，并原子推进目标用户授权版本。

use super::super::repository;
use async_trait::async_trait;
use yang_base::action::builtin::InsertResult;
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::table::Record;
use yang_base::{Action, BaseError};

#[derive(Action)]
#[action(
    name = "add",
    display_name = "新增企业成员",
    description = "在同一事务中新增成员并推进授权版本"
)]
pub(in crate::modules::org::user) struct AddMembershipAction;

#[async_trait]
impl TypedHandler for AddMembershipAction {
    type Input = Record;
    type Output = InsertResult;

    async fn handle(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        repository::add(&ctx, input).await
    }
}
