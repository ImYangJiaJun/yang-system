//! 展示静态 view_id 自定义页面覆盖。

use super::super::model::{DemoItems, NoInput};
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct InsightOutput {
    total: usize,
    active: usize,
    draft: usize,
}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
    items: DemoItems,
) -> Result<InsightOutput, BaseError> {
    let items = items.read().await;
    Ok(InsightOutput {
        total: items.len(),
        active: items.iter().filter(|item| item.status == "active").count(),
        draft: items.iter().filter(|item| item.status == "draft").count(),
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("insight"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Get, "/api/v1/demo/items/insight")
        .display_name("项目洞察")
        .description("展示静态 view_id 自定义页面覆盖")
        .public()
        .register()
}
