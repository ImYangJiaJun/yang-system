//! 验证附件下载不会被 JSON 解析。

use super::super::model::NoInput;
use std::path::PathBuf;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::BaseError;

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
    path: PathBuf,
) -> Result<ResponseBody, BaseError> {
    Ok(ResponseBody::download(path, "验收报告.txt"))
}
