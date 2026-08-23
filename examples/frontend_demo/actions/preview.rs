//! 验证浏览器内联预览通道。

use super::super::model::NoInput;
use std::path::PathBuf;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::BaseError;

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
    path: PathBuf,
) -> Result<ResponseBody, BaseError> {
    Ok(ResponseBody::preview(path))
}
