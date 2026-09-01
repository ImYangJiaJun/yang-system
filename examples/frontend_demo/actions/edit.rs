//! 通用行表单编辑演示。

use super::super::model::{DemoItems, MutationOutput};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) EditInput {
        id: Int::new().title("ID").require(true),
        name: Str::new().title("名称").require(true).max_length(100),
        category_id: Int::new().title("分类").require(true),
        status: Str::new().title("状态").require(true).max_length(20),
        parent_id: Int::new().title("父节点"),
    }
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: EditInput,
    items: DemoItems,
) -> Result<MutationOutput, BaseError> {
    let mut items = items.write().await;
    let item = items
        .iter_mut()
        .find(|item| item.id == input.id)
        .ok_or_else(|| BaseError::RecordNotFound(format!("项目 {} 不存在", input.id)))?;
    item.name = input.name;
    item.category_id = input.category_id;
    item.status = input.status;
    item.parent_id = input.parent_id;
    Ok(MutationOutput { id: input.id })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("edit"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Put, "/api/v1/demo/items")
        .display_name("编辑项目")
        .description("通用行表单编辑演示")
        .public()
        .register()
}
