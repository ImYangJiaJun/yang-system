//! `feishu.event` module：飞书事件订阅入口。
//!
//! 目前只含 challenge 应答端点。后续事件回调也归这里。

pub(crate) mod actions;

use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

use crate::config::FeishuSettings;

/// 本 module 的名字。
const MODULE: &str = "feishu.event";

/// 装配 `feishu.event` Module。
///
/// 无表、无认证中间件：challenge 端点是 public 且自校验（verification_token）。
pub(crate) fn build_module(
    settings: Option<&FeishuSettings>,
) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(module_name()?);
    let Some(settings) = settings else {
        return Ok(spec);
    };
    Ok(actions::register_all(spec, settings))
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(|error| BaseError::ConfigError(error.to_string()))
}
