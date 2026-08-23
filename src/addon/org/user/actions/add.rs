//! 新增企业成员，并原子推进目标用户授权版本。

use super::super::domain::repository;
use yang_base::action::builtin::InsertResult;
use yang_base::action::ActionContext;
use yang_base::table::Record;
use yang_base::BaseError;

pub(super) async fn handle(ctx: ActionContext, input: Record) -> Result<InsertResult, BaseError> {
    repository::add(&ctx, input).await
}
