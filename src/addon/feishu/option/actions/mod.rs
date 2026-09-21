//! `feishu.option` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod approval_options;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use super::super::domain::context::FeishuContext;

/// 注册本 module 的全部 Action。
pub(super) fn register_all(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    approval_options::register(module, context)
}
