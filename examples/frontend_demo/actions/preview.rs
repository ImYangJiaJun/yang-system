use super::super::model::NoInput;
use async_trait::async_trait;
use std::path::PathBuf;
use yang_base::action::{Action as ActionHandler, ActionContext, ResponseBody};
use yang_base::definition::ModuleSpec;
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "preview",
    display_name = "预览验收文件",
    description = "验证浏览器内联预览通道",
    method = "GET",
    path = "/api/v1/demo/preview",
    response_kind = "preview",
    public
)]
struct PreviewAction {
    path: PathBuf,
}

#[async_trait]
impl ActionHandler for PreviewAction {
    type Input = NoInput;
    type Output = ResponseBody;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(ResponseBody::preview(self.path.clone()))
    }
}

pub(super) fn register(module: ModuleSpec, path: PathBuf) -> ModuleSpec {
    module.native_action(PreviewAction { path })
}
