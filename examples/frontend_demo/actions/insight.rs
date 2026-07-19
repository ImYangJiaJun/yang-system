use super::super::model::{DemoItems, NoInput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::ModuleSpec;
use yang_base::{Action, BaseError};

#[derive(Debug, Serialize, JsonSchema)]
struct InsightOutput {
    total: usize,
    active: usize,
    draft: usize,
}

#[derive(Debug, Action)]
#[action(
    name = "insight",
    display_name = "项目洞察",
    description = "展示静态 view_id 自定义页面覆盖",
    method = "GET",
    path = "/api/v1/demo/items/insight",
    public
)]
struct InsightAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for InsightAction {
    type Input = NoInput;
    type Output = InsightOutput;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let items = self.items.read().await;
        Ok(InsightOutput {
            total: items.len(),
            active: items.iter().filter(|item| item.status == "active").count(),
            draft: items.iter().filter(|item| item.status == "draft").count(),
        })
    }
}

pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module.native_action(InsightAction { items })
}
