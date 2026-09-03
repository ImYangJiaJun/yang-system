//! 批量删除演示：验收 bulk placement 的选中行提交契约。
//!
//! 前端 bulk 语义（与旧前端一致）：把选中行整体作为 `selected` 数组提交；
//! 后端只提取每行的 `id`，容忍行内的其他字段。

use super::super::model::DemoItems;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, ParamInput, Params};
use yang_base::BaseError;

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct SelectedRow {
    id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BulkDeleteInput {
    selected: Vec<SelectedRow>,
}

impl ParamInput for BulkDeleteInput {
    fn params() -> Params {
        Params::new()
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BulkDeleteOutput {
    deleted: usize,
}

pub(super) async fn handle(
    _ctx: ActionContext,
    input: BulkDeleteInput,
    items: DemoItems,
) -> Result<BulkDeleteOutput, BaseError> {
    if input.selected.is_empty() {
        return Err(BaseError::ValidationFailed(
            "selected".to_string(),
            "批量删除至少选择一行".to_string(),
        ));
    }
    let ids: Vec<i64> = input.selected.iter().map(|row| row.id).collect();
    let mut items = items.write().await;
    let before = items.len();
    items.retain(|item| !ids.contains(&item.id));
    Ok(BulkDeleteOutput {
        deleted: before - items.len(),
    })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("bulk_delete"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Post, "/api/v1/demo/items/bulk-delete")
        .display_name("批量删除项目")
        .description("批量删除选中的项目")
        .public()
        .register()
}
