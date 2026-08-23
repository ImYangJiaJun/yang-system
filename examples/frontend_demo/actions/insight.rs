//! 展示静态 view_id 自定义页面覆盖。

use super::super::model::{DemoItems, NoInput};
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::ActionContext;
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
