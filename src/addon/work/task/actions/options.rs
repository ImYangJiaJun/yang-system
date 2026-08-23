//! 父任务关系选择 Action。

use std::sync::Arc;
use yang_base::action::builtin::RelationOptionsAction;
use yang_base::action::{Action, ActionContext};
use yang_base::table::{RelationOptionsRequest, RelationOptionsResponse};
use yang_base::BaseError;

pub(super) async fn handle(
    ctx: ActionContext,
    input: RelationOptionsRequest,
    inner: Arc<RelationOptionsAction>,
) -> Result<RelationOptionsResponse, BaseError> {
    inner.index(ctx, input).await
}
