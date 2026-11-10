//! `feishu.approval_export` Module：审批通过后回写多维表格的独立控制台。
//!
//! 回写与「审批派发」是两件事：派发负责从多维表格建审批单，回写负责在审批
//! 节点通过后把审批表单写入另一张多维表格。后端 Action、运行台账、worker
//! 都已独立；本 module 只负责把控制台入口放进侧边栏（app_route +
//! primary_action），不再借用 `feishu.approval` 的 tab。

pub(crate) mod actions;

use std::sync::Arc;

use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ModuleName, ModulePresentationSpec,
    ModuleSpec,
};
use yang_base::BaseError;

use super::datasource::with_authentication;
use super::domain::context::FeishuContext;
use crate::authorization::AuthorizationVersionValidator;
use crate::config::FeishuSettings;

/// 本 module 的名字。
const MODULE: &str = "feishu.approval_export";

/// 装配独立的「审批回写」控制台 Module。
///
/// 无表：规则 / 映射 / 运行台账三张表是各自独立的表 module，读写由本
/// module 的 Action 与回写 worker 经 `FeishuContext` 跨表完成。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let spec = with_authentication(ModuleSpec::new(module_name()?), authorization_validator);
    Ok(
        actions::register_all(spec, context, settings)?.presentation(
            ModulePresentationSpec::new(crate::addon::admin_identity(), "审批回写", "sync")
                .app_route("/feishu/approval-export")
                .order(45)
                .primary_action(yang_base::action!(
                    "feishu.approval_export.export_list_rules"
                ))
                .present_action(
                    yang_base::action!("feishu.approval_export.export_list_rules"),
                    ActionPresentationSpec::new(
                        ActionPlacement::Toolbar,
                        ActionInteraction::Invoke,
                    ),
                )
                .present_action(
                    yang_base::action!("feishu.approval_export.export_list_runs"),
                    ActionPresentationSpec::new(
                        ActionPlacement::Toolbar,
                        ActionInteraction::Invoke,
                    ),
                ),
        ),
    )
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(|error| BaseError::ConfigError(error.to_string()))
}
