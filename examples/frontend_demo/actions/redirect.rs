use super::super::model::NoInput;
use async_trait::async_trait;
use yang_base::action::{Action as ActionHandler, ActionContext, ResponseBody};
use yang_base::definition::ModuleSpec;
use yang_base::{Action, BaseError};

#[derive(Debug, Action)]
#[action(
    name = "redirect",
    display_name = "重定向验收",
    description = "验证前端展示 Location 而不是静默跳走",
    method = "GET",
    path = "/api/v1/demo/redirect",
    response_kind = "redirect",
    public
)]
struct RedirectAction;

#[async_trait]
impl ActionHandler for RedirectAction {
    type Input = NoInput;
    type Output = ResponseBody;

    async fn index(
        &self,
        _context: ActionContext,
        _input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(ResponseBody::redirect("/.well-known/yang/ui-catalog"))
    }
}

pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module.native_action(RedirectAction)
}
