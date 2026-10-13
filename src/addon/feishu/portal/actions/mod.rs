//! `feishu.portal` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod list_portals;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use crate::addon::feishu::domain::context::FeishuContext;

/// 注册本 module 的全部 Action。
///
/// 门户的增删改查都声明了 `feishu.portal.*` 权限并走框架 JWT 鉴权，
/// 因此与 `[feishu]` 段是否启用无关——控制台始终可用来准备门户。
pub(super) fn register_all(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    list_portals::register(module, context)
}
