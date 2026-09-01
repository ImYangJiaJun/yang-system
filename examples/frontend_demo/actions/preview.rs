//! 验证浏览器内联预览通道。

use super::super::model::NoInput;
use std::path::PathBuf;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{ActionResponseKind, HttpMethod, ModuleSpec};
use yang_base::BaseError;

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
    path: PathBuf,
) -> Result<ResponseBody, BaseError> {
    Ok(ResponseBody::preview(path))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, fixture: PathBuf) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("preview"), move |ctx, input| {
            handle(ctx, input, fixture.clone())
        })
        .route(HttpMethod::Get, "/api/v1/demo/preview")
        .display_name("预览验收文件")
        .description("验证浏览器内联预览通道")
        .public()
        .response_kind(ActionResponseKind::Preview)
        .register()
}
