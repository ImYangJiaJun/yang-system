//! `feishu.option` Module：选项数据。
//!
//! 本 module 同时承载三类入口，用「谁在用」区分：
//! - 控制台**只读**查询（`list_options`，受保护 Action）
//! - 飞书审批取选项（`approval_options`，public + 按数据源 Token 自校验）
//! - 多维表格写入（`upsert_options` / `delete_options`，public + 管理 Token 中间件）

pub(crate) mod actions;
pub(crate) mod table;

use std::sync::Arc;

use yang_base::definition::{
    ActionInteraction, ActionName, ActionPlacement, ActionPresentationSpec, ActionRef, FieldName,
    FieldRef, ModuleName, ModulePresentationSpec, ModuleSpec, SortDirection, TableName,
    TableSortSpec, ViewName, ViewSpec,
};
use yang_base::BaseError;

use super::domain::context::FeishuContext;
use crate::addon::feishu::datasource::with_authentication;
use crate::authorization::AuthorizationVersionValidator;
use crate::config::FeishuSettings;

/// 本 module 的名字。
const MODULE: &str = "feishu.option";
/// 本 module 的表名。
const TABLE: &str = "feishu_option";

/// 装配 `feishu.option` Module。
pub(crate) fn build_module(
    context: Arc<FeishuContext>,
    settings: Option<&FeishuSettings>,
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let spec = ModuleSpec::new(module_name()?)
        .table(table::table_spec()?)
        .presentation(presentation())
        .view(view()?);
    // 与 datastore 模块同一套认证中间件：本 module 的受保护 Action（list_options）
    // 要靠它才有身份；而 public 的机器入口在完全不带 Authorization 头时放行匿名。
    let spec = with_authentication(spec, authorization_validator);
    actions::register_all(spec, context, settings)
}

/// 前端展示投影（控制台导航）。
fn presentation() -> ModulePresentationSpec {
    ModulePresentationSpec::new(crate::addon::user_identity(), "飞书选项", "list")
        .description("飞书审批外部选项的数据")
        .order(41)
        .primary_action(yang_base::action!("feishu.option.list_options"))
}

/// 通用 TableView。
///
/// 只挂**查询**操作：选项的增改由多维表格写入 API 承担，控制台保持只读——
/// 两个并存的可写入口会让审计语义与数据来源分叉。
fn view() -> Result<ViewSpec, BaseError> {
    let field = |name: &str| -> Result<FieldRef, BaseError> {
        Ok(FieldRef::new(
            TableName::new(TABLE).map_err(config_error)?,
            FieldName::new(name).map_err(config_error)?,
        ))
    };
    let action = |name: &str| -> Result<ActionRef, BaseError> {
        Ok(ActionRef::new(
            module_name()?,
            ActionName::new(name).map_err(config_error)?,
        ))
    };

    Ok(ViewSpec::new(ViewName::new("main").map_err(config_error)?)
        .title("选项")
        .data_action(action("list_options")?)
        .field(field("option_id")?)
        .field(field("source_key")?)
        .field(field("label")?)
        .field(field("sort_order")?)
        .field(field("enabled")?)
        .action(action("list_options")?)
        .present_action(
            action("list_options")?,
            ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Invoke),
        )
        .default_sort(TableSortSpec::new(field("sort_order")?, SortDirection::Asc)))
}

fn module_name() -> Result<ModuleName, BaseError> {
    ModuleName::new(MODULE).map_err(config_error)
}

fn config_error(error: impl std::fmt::Display) -> BaseError {
    BaseError::ConfigError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_name_and_table_are_declared() {
        let name = module_name().unwrap_or_else(|error| panic!("模块名应有效: {error}"));
        assert_eq!(name.as_str(), MODULE);
        assert_eq!(TABLE, "feishu_option");
    }

    #[test]
    fn view_is_read_only_and_projects_the_core_columns() {
        let spec = view().unwrap_or_else(|error| panic!("View 应可构造: {error}"));
        assert_eq!(spec.fields.len(), 5);
        assert_eq!(spec.actions.len(), 1, "控制台对选项只读：不应挂任何写操作");
        assert_eq!(spec.actions[0].action().as_str(), "list_options");
    }
}
