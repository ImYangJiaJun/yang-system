//! 验证前端展示 Location 而不是静默跳走。

use super::super::model::NoInput;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::BaseError;

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
) -> Result<ResponseBody, BaseError> {
    Ok(ResponseBody::redirect("/.well-known/yang/ui-catalog"))
}
