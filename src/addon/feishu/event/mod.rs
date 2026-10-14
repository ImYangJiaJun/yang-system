//! `feishu.event` module：飞书事件订阅入口。
//!
//! 含 challenge 应答端点（URL 验证）和事件接收端点（落事件落地表）。

pub(crate) mod actions;

use std::sync::Arc;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;
use crate::config::FeishuSettings;

/// 本 module 的名字。
const MODULE: &str = "feishu.event";

/// 装配 `feishu.event` Module。
///
/// 无表、无认证中间件：两个端点都是 public 且自校验（verification_token）。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(module_name()?);
    let Some(settings) = settings else {
        return Ok(spec);
    };
    Ok(actions::register_all(spec, context, settings))
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(|error| BaseError::ConfigError(error.to_string()))
}
