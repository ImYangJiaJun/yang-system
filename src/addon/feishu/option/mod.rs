//! `feishu.option` Module：选项数据。
//!
//! 本文件是模块的"定义卡"：表、Action 注册表按分区顺序装配。
//! 当前尚无 Action——它们连同展示投影在后续任务落地。

pub(crate) mod actions;
pub(crate) mod table;

use std::sync::Arc;

use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

use super::domain::context::FeishuContext;

/// 装配 `feishu.option` Module。
pub(crate) fn build_module(context: Arc<FeishuContext>) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(
        ModuleName::new("feishu.option")
            .map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(table::table_spec()?);
    Ok(actions::register_all(spec, context))
}
