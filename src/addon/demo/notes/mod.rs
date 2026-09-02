//! `demo.notes` Module（module 层）：便签模块装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表、
//! 展示投影与通用 TableView View 按分区顺序装配；业务用例全部在
//! `actions/` 的自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::context::Demo;
use super::domain::repository::NoteRepository;
use crate::addon::account::user_from_claims;
use crate::authorization::AuthorizationVersionValidator;
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{
    ActionConfirmation, ActionInteraction, ActionName, ActionPlacement, ActionPresentationSpec,
    ActionRef, FieldName, FieldRef, ModuleName, ModulePresentationSpec, ModuleSpec, SortDirection,
    TableName, TableSortSpec, ViewName, ViewSpec,
};
use yang_base::BaseError;

/// 装配 `demo.notes` Module：表 → 上下文 → 中间件 → Action 注册表 → 展示投影 + TableView。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
) -> Result<ModuleSpec, BaseError> {
    let table = table::notes_table_spec()?;
    let demo = Arc::new(Demo::new(NoteRepository::new(table.table_definition()?)));

    let mut module = ModuleSpec::new(
        ModuleName::new("demo.notes").map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .table(table)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    module = actions::register_all(module, demo);
    Ok(module.presentation(presentation()).view(view()?))
}

/// 前端展示投影（便签导航）。
fn presentation() -> ModulePresentationSpec {
    ModulePresentationSpec::new(crate::addon::user_identity(), "便签", "demo")
        .description("记录与管理当前用户的个人便签")
        .order(30)
        .primary_action(yang_base::action!("demo.notes.list_notes"))
}

/// 通用 TableView：数据 Action + 列 + 行/工具栏操作全部声明式投影，
/// 前端 ModulePage 无需任何业务代码即可渲染完整 CRUD 页面。
fn view() -> Result<ViewSpec, BaseError> {
    let table_name = TableName::new(table::TABLE_NAME)
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let module_name =
        ModuleName::new("demo.notes").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    let field_ref = |name: &str| -> Result<FieldRef, BaseError> {
        let field =
            FieldName::new(name).map_err(|error| BaseError::ConfigError(error.to_string()))?;
        Ok(FieldRef::new(table_name.clone(), field))
    };
    let action_ref = |name: &str| -> Result<ActionRef, BaseError> {
        let action =
            ActionName::new(name).map_err(|error| BaseError::ConfigError(error.to_string()))?;
        Ok(ActionRef::new(module_name.clone(), action))
    };

    Ok(ViewSpec::new(
        ViewName::new("main").map_err(|error| BaseError::ConfigError(error.to_string()))?,
    )
    .title("便签列表")
    .data_action(action_ref("list_notes")?)
    .field(field_ref(table::NOTE_ID)?)
    .field(field_ref(table::TITLE)?)
    .field(field_ref(table::CONTENT)?)
    .field(field_ref(table::CREATED_AT)?)
    .field(field_ref(table::UPDATED_AT)?)
    .action(action_ref("list_notes")?)
    .present_action(
        action_ref("create_note")?,
        ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
    )
    .present_action(
        action_ref("update_note")?,
        ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Form)
            .record_parameter("id"),
    )
    .present_action(
        action_ref("delete_note")?,
        ActionPresentationSpec::new(ActionPlacement::Row, ActionInteraction::Invoke)
            .record_parameter("id")
            .confirmation(ActionConfirmation::new(
                "删除便签",
                "删除后不可恢复，确认删除这条便签？",
            )),
    )
    .default_sort(TableSortSpec::new(
        field_ref(table::CREATED_AT)?,
        SortDirection::Desc,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::AuthorizationVersionValidator;

    fn built_module() -> ModuleSpec {
        build_module(AuthorizationVersionValidator::new(None, None))
            .unwrap_or_else(|error| panic!("demo.notes 模块应装配成功: {error}"))
    }

    #[test]
    fn every_note_action_declares_its_permission() {
        let module = built_module();
        let expected = [
            ("create_note", "demo.notes.write"),
            ("update_note", "demo.notes.write"),
            ("delete_note", "demo.notes.write"),
            ("list_notes", "demo.notes.read"),
        ];
        for (action_name, permission) in expected {
            let action = module
                .actions()
                .iter()
                .find(|action| action.name.as_str() == action_name)
                .unwrap_or_else(|| panic!("应注册 {action_name}"));
            assert!(
                action.permissions.iter().any(|item| item == permission),
                "{action_name} 必须声明权限 {permission}"
            );
        }
    }

    #[test]
    fn view_projects_table_view_with_owner_scoped_data_action() {
        let module = built_module();
        assert_eq!(module.views.len(), 1, "便签模块应声明一个主 TableView");
        let view = &module.views[0];
        let data_action = view
            .data_action
            .as_ref()
            .unwrap_or_else(|| panic!("TableView 必须声明数据 Action"));
        assert_eq!(data_action.action().as_str(), "list_notes");
        assert_eq!(
            view.fields.len(),
            5,
            "View 应投影 id/title/content/created_at/updated_at"
        );
        for presented in ["create_note", "update_note", "delete_note"] {
            assert!(
                view.actions
                    .iter()
                    .any(|action| action.action().as_str() == presented),
                "View 应展示 {presented}"
            );
        }
        assert_eq!(view.default_sort.len(), 1);
    }
}
