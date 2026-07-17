//! 强类型内部 Action 调用样板：构建期绑定 slot，请求期零 JSON 往返。

use super::register::RegisterInput;
use super::UserView;
use async_trait::async_trait;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ActionLink, ActionName, ActionRef, ModuleName, ModuleSpec, Registry};
use yang_base::{Action, BaseError};

#[derive(Action)]
#[action(
    name = "register_via_plugin",
    display_name = "通过内部 Action 注册",
    description = "演示 Plugins 强类型内部调用",
    method = "POST",
    path = "/api/v1/users/register-via-plugin",
    success_status = 201,
    public
)]
struct RegisterViaPluginAction {
    register: ActionLink<RegisterInput, UserView>,
}

#[async_trait]
impl ActionHandler for RegisterViaPluginAction {
    type Input = RegisterInput;
    type Output = UserView;

    fn calls(&self) -> Vec<ActionRef> {
        vec![self.register.reference().clone()]
    }

    fn bind_registry(&self, registry: &Registry) -> Result<(), BaseError> {
        self.register.bind(registry)
    }

    async fn index(
        &self,
        ctx: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        ctx.plugins()?.api_run(self.register.handle()?, input).await
    }
}

pub(super) fn register(module: ModuleSpec) -> Result<ModuleSpec, BaseError> {
    let target = ActionRef::new(
        ModuleName::new("account.user")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
        ActionName::new("register").map_err(|error| BaseError::ConfigError(error.to_string()))?,
    );
    Ok(module.native_action(RegisterViaPluginAction {
        register: ActionLink::new(target),
    }))
}
