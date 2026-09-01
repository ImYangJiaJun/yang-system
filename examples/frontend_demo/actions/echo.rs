//! 用于验收默认 ActionDemo 的真实 HTTP 调用。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec, Str};
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

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("echo"), handle)
        .route(HttpMethod::Post, "/api/v1/demo/echo")
        .display_name("回显输入")
        .description("用于验收默认 ActionDemo 的真实 HTTP 调用")
        .public()
        .register()
}
