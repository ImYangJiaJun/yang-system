use super::super::model::{DemoItems, MutationOutput};
use async_trait::async_trait;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    EditInput {
        id: Int::new().title("ID").require(true),
        name: Str::new().title("名称").require(true).max_length(100),
        category_id: Int::new().title("分类").require(true),
        status: Str::new().title("状态").require(true).max_length(20),
        parent_id: Int::new().title("父节点"),
    }
}

#[derive(Debug, Action)]
#[action(
    name = "edit",
    display_name = "编辑项目",
    description = "通用行表单编辑演示",
    method = "PUT",
    path = "/api/v1/demo/items",
    public
)]
struct EditAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for EditAction {
    type Input = EditInput;
    type Output = MutationOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let mut items = self.items.write().await;
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
}

pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module.native_action(EditAction { items })
}
