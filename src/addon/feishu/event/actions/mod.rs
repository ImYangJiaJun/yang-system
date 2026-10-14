//! `feishu.event` module 的 Action 注册表。

pub(super) mod event_challenge;
pub(super) mod receive_approval_event;

use std::sync::Arc;
use yang_base::definition::ModuleSpec;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::config::FeishuSettings;

/// 注册本 module 的全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    context: Arc<FeishuContext>,
    settings: &FeishuSettings,
) -> ModuleSpec {
    let module = event_challenge::register(module, (*context).clone(), settings);
    receive_approval_event::register(
        module,
        Arc::try_unwrap(context).unwrap_or_else(|arc| (*arc).clone()),
        settings,
    )
}
