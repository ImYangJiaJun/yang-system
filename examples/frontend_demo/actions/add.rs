use super::super::model::{DemoItem, DemoItems, MutationOutput};
use async_trait::async_trait;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    AddInput {
        name: Str::new().title("名称").require(true).max_length(100),
        category_id: Int::new().title("分类").require(true),
        status: Str::new().title("状态").require(true).max_length(20),
        parent_id: Int::new().title("父节点"),
    }
}

#[derive(Debug, Action)]
#[action(
    name = "add",
    display_name = "新增项目",
    description = "通用表单新增演示",
    method = "POST",
    path = "/api/v1/demo/items",
    public
)]
struct AddAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for AddAction {
    type Input = AddInput;
    type Output = MutationOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let mut items = self.items.write().await;
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
}

pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module.native_action(AddAction { items })
}
