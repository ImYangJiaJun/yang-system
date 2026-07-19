use super::super::model::{DemoItems, MutationOutput};
use async_trait::async_trait;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{Int, ModuleSpec};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    DeleteInput {
        id: Int::new().title("ID").require(true),
    }
}

#[derive(Debug, Action)]
#[action(
    name = "delete",
    display_name = "删除项目",
    description = "通用确认调用演示",
    method = "DELETE",
    path = "/api/v1/demo/items",
    public
)]
struct DeleteAction {
    items: DemoItems,
}

#[async_trait]
impl ActionHandler for DeleteAction {
    type Input = DeleteInput;
    type Output = MutationOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        let mut items = self.items.write().await;
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
}

pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module.native_action(DeleteAction { items })
}
