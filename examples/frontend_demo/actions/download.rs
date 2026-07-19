use super::super::model::NoInput;
use async_trait::async_trait;
use std::path::PathBuf;
use yang_base::action::{Action as ActionHandler, ActionContext, ResponseBody};
use yang_base::definition::ModuleSpec;
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "download",
    display_name = "下载验收文件",
    description = "验证附件下载不会被 JSON 解析",
    method = "GET",
    path = "/api/v1/demo/download",
    response_kind = "download",
    public
)]
struct DownloadAction {
    path: PathBuf,
}

#[async_trait]
impl ActionHandler for DownloadAction {
    type Input = NoInput;
    type Output = ResponseBody;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(ResponseBody::download(self.path.clone(), "验收报告.txt"))
    }
}

pub(super) fn register(module: ModuleSpec, path: PathBuf) -> ModuleSpec {
    module.native_action(DownloadAction { path })
}
