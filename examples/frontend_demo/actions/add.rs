//! 通用表单新增演示。

use super::super::model::{DemoItem, DemoItems, MutationOutput};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, Int, ModuleSpec, Str};
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) AddInput {
        name: Str::new().title("名称").require(true).max_length(100),
        category_id: Int::new().title("分类").require(true),
        status: Str::new().title("状态").require(true).max_length(20),
        parent_id: Int::new().title("父节点"),
    }
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: AddInput,
    items: DemoItems,
) -> Result<MutationOutput, BaseError> {
    let mut items = items.write().await;
    let id = items.iter().map(|item| item.id).max().unwrap_or(0) + 1;
    items.push(DemoItem {
        id,
        name: input.name,
        category_id: input.category_id,
        status: input.status,
        parent_id: input.parent_id,
    });
    Ok(MutationOutput { id })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("add"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Post, "/api/v1/demo/items")
        .display_name("新增项目")
        .description("通用表单新增演示")
        .public()
        .register()
}
