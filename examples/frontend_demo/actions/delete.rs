//! 通用确认调用演示。

use super::super::model::{DemoItems, MutationOutput};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) DeleteInput {
        id: Int::new().title("ID").require(true),
    }
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: DeleteInput,
    items: DemoItems,
) -> Result<MutationOutput, BaseError> {
    let mut items = items.write().await;
    let before = items.len();
    items.retain(|item| item.id != input.id);
    if items.len() == before {
        return Err(BaseError::RecordNotFound(format!(
            "项目 {} 不存在",
            input.id
        )));
    }
    Ok(MutationOutput { id: input.id })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("delete"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Delete, "/api/v1/demo/items")
        .display_name("删除项目")
        .description("通用确认调用演示")
        .public()
        .register()
}
