//! 重置演示数据：E2E 每个用例从同一份种子开始，不依赖执行顺序与重试。
//!
//! 只服务于验收后端；注册为普通 Action 但不在 View 的 `present_action` 里，
//! 所以控制台不会出现入口。

use super::super::model::{fixture, DemoItems, NoInput};
use schemars::JsonSchema;
use serde::Serialize;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::definition::{HttpMethod, ModuleSpec};
use yang_base::BaseError;

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ResetOutput {
    reset: bool,
}

pub(super) async fn handle(
    _ctx: ActionContext,
    _input: NoInput,
    items: DemoItems,
) -> Result<ResetOutput, BaseError> {
    *items.write().await = fixture();
    Ok(ResetOutput { reset: true })
}

/// 自包含注册：路由/展示元数据与 Handler 在同一文件内原子绑定。
pub(super) fn register(module: ModuleSpec, items: DemoItems) -> ModuleSpec {
    module
        .action_fn(yang_base::action_name!("reset"), move |ctx, input| {
            handle(ctx, input, Arc::clone(&items))
        })
        .route(HttpMethod::Post, "/api/v1/demo/items/reset")
        .display_name("重置演示数据")
        .description("E2E 用例的种子数据重置")
        .public()
        .register()
}
