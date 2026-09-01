//! 验证前端展示 Location 而不是静默跳走。

use super::super::model::NoInput;
use yang_base::action::{ActionContext, ResponseBody};
use yang_base::definition::{ActionResponseKind, HttpMethod, ModuleSpec};
use yang_base::BaseError;

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
) -> Result<ResponseBody, BaseError> {
    Ok(ResponseBody::redirect("/.well-known/yang/ui-catalog"))
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("redirect"), handle)
        .route(HttpMethod::Get, "/api/v1/demo/redirect")
        .display_name("重定向验收")
        .description("验证前端展示 Location 而不是静默跳走")
        .public()
        .response_kind(ActionResponseKind::Redirect)
        .register()
}
