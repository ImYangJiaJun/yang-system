//! `feishu.datasource` module 的 Action 注册表。
//!
//! 架构门禁要求：`actions/` 下每个文件恰好一个 `pub(super) async fn handle` +
//! 一个 `pub(super) fn register`，并在这里登记。

pub(super) mod create_datasource;
pub(super) mod delete_datasource;
pub(super) mod list_bitable_fields;
pub(super) mod list_bitable_tables;
pub(super) mod list_bitable_views;
pub(super) mod list_datasources;
pub(super) mod pull_now;
pub(super) mod pull_probe;
pub(super) mod pull_schedule;
pub(super) mod update_datasource;

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use crate::addon::feishu::domain::context::FeishuContext;

/// 注册本 module 的全部 Action。
///
/// 数据源的增删改查都声明了 `feishu.datasource.*` 权限并走框架 JWT 鉴权，
/// 因此与 `[feishu]` 段是否启用无关——控制台始终可用来准备数据源。
///
/// **例外是三个出站相关端点**：`pull_probe`（探针）、`pull_now`（立即拉取）、
/// `pull_schedule`（排程）。它们都只在 `can_pull()`（凭证齐备且非占位）时才注册——
/// 没有 worker 的时候，前两个只能返回「Worker 未在运行」，第三个只能答「不知道」，
/// 注册出来都是让人以为可用。理由与 `option` module 的机器入口一致。
pub(super) fn register_all(module: ModuleSpec, context: Arc<FeishuContext>) -> ModuleSpec {
    let module = list_datasources::register(module, Arc::clone(&context));
    let module = list_bitable_tables::register(module, Arc::clone(&context));
    let module = list_bitable_views::register(module, Arc::clone(&context));
    let module = list_bitable_fields::register(module, Arc::clone(&context));
    let module = create_datasource::register(module, Arc::clone(&context));
    let module = update_datasource::register(module, Arc::clone(&context));
    let module = delete_datasource::register(module, Arc::clone(&context));

    if context
        .settings()
        .is_some_and(|settings| settings.can_pull())
    {
        let module = pull_probe::register(module, Arc::clone(&context));
        let module = pull_now::register(module, Arc::clone(&context));
        return pull_schedule::register(module, context);
    }
    module
}
