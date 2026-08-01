//! 删除企业成员，并原子推进目标用户授权版本。

use super::super::repository;
use async_trait::async_trait;
use yang_base::action::builtin::{AffectedResult, GetByPk};
use yang_base::action::{ActionContext, TypedHandler};
use yang_base::{Action, BaseError};

#[derive(Action)]
#[action(
    name = "del",
    display_name = "删除企业成员",
    description = "在同一事务中删除成员并推进授权版本"
)]
pub(in crate::addon::org::user) struct DeleteMembershipAction;

#[async_trait]
impl TypedHandler for DeleteMembershipAction {
    type Input = GetByPk;
    type Output = AffectedResult;

    async fn handle(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        repository::delete(&ctx, input).await
    }
}
