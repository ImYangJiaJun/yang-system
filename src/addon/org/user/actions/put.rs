//! 修改企业成员，并原子推进所有受影响用户授权版本。

use super::super::repository;
use async_trait::async_trait;
use yang_base::action::builtin::{AffectedResult, PutInput};
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::{Action, BaseError};

#[derive(Action)]
#[action(
    name = "put",
    display_name = "修改企业成员",
    description = "在同一事务中修改成员并推进授权版本"
)]
pub(in crate::addon::org::user) struct PutMembershipAction;

#[async_trait]
impl TypedHandler for PutMembershipAction {
    type Input = PutInput;
    type Output = AffectedResult;

    async fn handle(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        repository::put(&ctx, input).await
    }
}
