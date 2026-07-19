use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{Action as ActionHandler, ActionContext, UploadedFile};
use yang_base::definition::{ModuleSpec, ParamInput, Params};
use yang_base::{Action, BaseError};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UploadInput {
    title: String,
    file: UploadedFile,
}

impl ParamInput for UploadInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct UploadOutput {
    title: String,
    filename: String,
    content_type: String,
    size: u64,
    content: String,
}

#[derive(Debug, Action)]
#[action(
    name = "upload",
    display_name = "上传验收文件",
    description = "验证受限 multipart 表单与请求作用域文件",
    method = "POST",
    path = "/api/v1/demo/upload",
    public,
    request_media = "multipart",
    content_types("text/plain"),
    max_fields = 1,
    max_files = 1,
    max_file_bytes = 1024,
    max_total_bytes = 131072
)]
struct UploadAction;

#[async_trait]
impl ActionHandler for UploadAction {
    type Input = UploadInput;
    type Output = UploadOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let content = tokio::fs::read_to_string(input.file.path()).await?;
        Ok(UploadOutput {
            title: input.title,
            filename: input.file.original_filename().to_string(),
            content_type: input.file.content_type().to_string(),
            size: input.file.size(),
            content,
        })
    }
}

pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module.native_action(UploadAction)
}
