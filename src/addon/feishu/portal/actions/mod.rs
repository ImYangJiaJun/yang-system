//! `feishu.portal` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod create_portal;
pub(super) mod delete_portal;
pub(super) mod export_portal_results;
pub(super) mod list_portals;
pub(super) mod portal_access_log;
pub(super) mod portal_metadata;
pub(super) mod portal_query;
pub(super) mod set_passphrase;
pub(super) mod toggle_portal;
pub(super) mod update_portal;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use crate::addon::feishu::domain::context::FeishuContext;

/// 注册本 module 的全部 Action。
///
/// 门户的增删改查都声明了 `feishu.portal.*` 权限并走框架 JWT 鉴权，
/// 因此与 `[feishu]` 段是否启用无关——控制台始终可用来准备门户。
/// 例外是公开侧的 `portal_metadata`（取标题等配置）与 `portal_query`（查数据）：
/// 两者都 `public()`，判权在 handler 内按门户的访问模式完成。
pub(super) fn register_all(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    let module = list_portals::register(module, Arc::clone(&context));
    let module = create_portal::register(module, Arc::clone(&context));
    let module = update_portal::register(module, Arc::clone(&context));
    let module = delete_portal::register(module, Arc::clone(&context));
    let module = toggle_portal::register(module, Arc::clone(&context));
    let module = set_passphrase::register(module, Arc::clone(&context));
    let module = portal_access_log::register(module, Arc::clone(&context));
    let module = export_portal_results::register(module, Arc::clone(&context));
    let module = portal_metadata::register(module, Arc::clone(&context));
    portal_query::register(module, context)
}
