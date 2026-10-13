//! `feishu.event` module 的 Action 注册表。

pub(super) mod event_challenge;

use yang_base::definition::ModuleSpec;

use crate::config::FeishuSettings;

/// 注册本 module 的全部 Action。
pub(super) fn register_all(
    module: ModuleSpec,
    settings: &FeishuSettings,
) -> ModuleSpec {
    event_challenge::register(module, settings)
}
