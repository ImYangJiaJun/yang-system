use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::{Action as ActionHandler, ActionContext};
use yang_base::definition::{ModuleSpec, Str};
use yang_base::{Action, BaseError};

yang_base::params! {
    #[deny_unknown_fields]
    EchoInput {
        message: Str::new()
            .title("消息")
            .description("服务端会原样返回该文本")
            .require(true)
            .min_length(1)
            .max_length(200),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct EchoOutput {
    message: String,
    length: usize,
}

#[derive(Debug, Action)]
#[action(
    name = "echo",
    display_name = "回显输入",
    description = "用于验收默认 ActionDemo 的真实 HTTP 调用",
    method = "POST",
    path = "/api/v1/demo/echo",
    public
)]
struct EchoAction;

#[async_trait]
impl ActionHandler for EchoAction {
    type Input = EchoInput;
    type Output = EchoOutput;

    async fn index(
        &self,
        _context: ActionContext,
        input: Self::Input,
    ) -> Result<Self::Output, BaseError> {
        Ok(EchoOutput {
            length: input.message.chars().count(),
            message: input.message,
        })
    }
}

pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module.native_action(EchoAction)
}
