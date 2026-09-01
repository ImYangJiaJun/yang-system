//! 验证受限 multipart 表单与请求作用域文件。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use yang_base::action::{ActionContext, UploadedFile};
use yang_base::definition::{HttpMethod, ModuleSpec, MultipartSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadInput {
    title: String,
    file: UploadedFile,
}

impl ParamInput for UploadInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct UploadOutput {
    title: String,
    filename: String,
    content_type: String,
    size: u64,
    content: String,
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: UploadInput,
) -> Result<UploadOutput, BaseError> {
    let content = tokio::fs::read_to_string(input.file.path()).await?;
    Ok(UploadOutput {
        title: input.title,
        filename: input.file.original_filename().to_string(),
        content_type: input.file.content_type().to_string(),
        size: input.file.size(),
        content,
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("upload"), handle)
        .route(HttpMethod::Post, "/api/v1/demo/upload")
        .display_name("上传验收文件")
        .description("验证受限 multipart 表单与请求作用域文件")
        .public()
        .multipart(
            MultipartSpec::new(["text/plain"])
                .max_fields(1)
                .max_files(1)
                .max_file_bytes(1024)
                .max_total_bytes(131072),
        )
        .register()
}
