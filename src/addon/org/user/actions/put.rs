//! 修改企业成员，并原子推进所有受影响用户授权版本。

use super::super::domain::repository;
use yang_base::action::builtin::{AffectedResult, PutInput};
use yang_base::action::ActionContext;
use yang_base::BaseError;

pub(super) async fn handle(
    ctx: ActionContext,
    input: PutInput,
) -> Result<AffectedResult, BaseError> {
    repository::put(&ctx, input).await
}
