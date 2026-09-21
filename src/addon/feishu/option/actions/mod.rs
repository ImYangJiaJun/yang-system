//! 选项 module 的 Action 注册表。

use std::sync::Arc;

use yang_base::definition::ModuleSpec;

use super::super::domain::context::FeishuContext;

/// 空注册表，保持 module 形状合法（Action 在后续任务填充）。
pub(super) fn register_all(module: ModuleSpec, _context: Arc<FeishuContext>) -> ModuleSpec {
    module
}
