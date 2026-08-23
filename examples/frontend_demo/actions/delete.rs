//! 通用确认调用演示。

use super::super::model::{DemoItems, MutationOutput};
use yang_base::action::ActionContext;
use yang_base::definition::Int;
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
