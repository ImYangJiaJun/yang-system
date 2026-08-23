//! 用于验收默认 ActionDemo 的真实 HTTP 调用。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::ActionContext;
use yang_base::definition::Str;
use yang_base::BaseError;

yang_base::params! {
    #[deny_unknown_fields]
    pub(super) EchoInput {
        message: Str::new()
            .title("消息")
            .description("服务端会原样返回该文本")
            .require(true)
            .min_length(1)
            .max_length(200),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct EchoOutput {
    message: String,
    length: usize,
}

pub(super) async fn handle(_ctx: ActionContext, input: EchoInput) -> Result<EchoOutput, BaseError> {
    Ok(EchoOutput {
        length: input.message.chars().count(),
        message: input.message,
    })
}
