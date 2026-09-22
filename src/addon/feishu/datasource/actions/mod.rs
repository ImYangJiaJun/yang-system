//! `feishu.datasource` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod create_datasource;
pub(super) mod delete_datasource;
pub(super) mod list_datasources;
pub(super) mod pull_probe;
pub(super) mod update_datasource;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use crate::addon::feishu::domain::context::FeishuContext;

/// 注册本 module 的全部 Action。
///
/// 数据源的增删改查都声明了 `feishu.datasource.*` 权限并走框架 JWT 鉴权，
/// 因此与 `[feishu]` 段是否启用无关——控制台始终可用来准备数据源。
///
/// **唯一的例外是出站拉取探针**：它要出站调飞书并消耗频控配额，因此只在
/// `can_pull()`（凭证齐备且非占位）时才注册。理由与 `option` module 的机器入口
/// 一致——注册出来却在每次调用时拒绝，会让人以为它可用。
pub(super) fn register_all(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    let module = list_datasources::register(module, Arc::clone(&context));
    let module = create_datasource::register(module, Arc::clone(&context));
    let module = update_datasource::register(module, Arc::clone(&context));
    let module = delete_datasource::register(module, Arc::clone(&context));

    if context
        .settings()
        .is_some_and(|settings| settings.can_pull())
    {
        return pull_probe::register(module, context);
    }
    module
}
