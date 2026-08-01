//! `work.task` Module：具备树、关系、批量操作和双 View 的任务模型。

mod actions;
mod repository;
mod view;

use yang_base::action::builtin::DelAction;
use yang_base::definition::{
    Fields, Module, ModuleName, ModulePresentationSpec, ModuleSpec, Radio, Str, Table, TableName,
    TableSpec, Timestamp,
};
use yang_base::BaseError;

struct WorkTaskModule;

impl Module for WorkTaskModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("work.task")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("work_task"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => yang_base::definition::Key::new().title("ID").filterable(true),
            owner_user => Table::new()
                .title("个人工作区")
                .require(true)
                .target(yang_base::field!("users.id"))
                .display([yang_base::field!("users.username")])
                .tenant_key(true)
                .filterable(true),
            project_project => Table::new()
                .title("所属项目")
                .require(true)
                .target(yang_base::field!("work_project.id"))
                .display([yang_base::field!("work_project.name")])
                .select(yang_base::action!("work.project.options"))
                .filterable(true),
            parent_task => Table::new()
                .title("父任务")
                .target(yang_base::field!("work_task.id"))
                .display([yang_base::field!("work_task.title")])
                .select(yang_base::action!("work.task.options"))
                .filterable(true),
            title => Str::new()
                .title("任务标题")
                .require(true)
                .max_length(160)
                .searchable(true)
                .sortable(true),
            status => Radio::<String>::new()
                .title("任务状态")
                .require(true)
                .options([("todo", "待处理"), ("doing", "进行中"), ("done", "已完成")])
                .default("todo")
                .filterable(true)
                .sortable(true),
            priority => Radio::<String>::new()
                .title("优先级")
                .require(true)
                .options([("low", "低"), ("normal", "普通"), ("high", "高")])
                .default("normal")
                .filterable(true)
                .sortable(true),
            due_at => Timestamp::new().title("截止时间").filterable(true).sortable(true),
            created_at => Timestamp::new().title("创建时间").created_at().sortable(true),
            updated_at => Timestamp::new().title("更新时间").updated_at().sortable(true),
        }
    }

    fn configure_table(&self, table: TableSpec) -> TableSpec {
        table
            .unique_named(
                "uk_work_task_id_project_owner",
                [
                    yang_base::field!("work_task.id"),
                    yang_base::field!("work_task.project_project"),
                    yang_base::field!("work_task.owner_user"),
                ],
            )
            .index_named(
                "idx_work_task_owner_project_status",
                [
                    yang_base::field!("work_task.owner_user"),
                    yang_base::field!("work_task.project_project"),
                    yang_base::field!("work_task.status"),
                ],
            )
            .index_named(
                "idx_work_task_owner_parent",
                [
                    yang_base::field!("work_task.owner_user"),
                    yang_base::field!("work_task.parent_task"),
                ],
            )
            .foreign_key_named(
                "fk_work_task_owner",
                [yang_base::field!("work_task.owner_user")],
                [yang_base::field!("users.id")],
            )
            .foreign_key_named(
                "fk_work_task_project_owner",
                [
                    yang_base::field!("work_task.project_project"),
                    yang_base::field!("work_task.owner_user"),
                ],
                [
                    yang_base::field!("work_project.id"),
                    yang_base::field!("work_project.owner_user"),
                ],
            )
            .foreign_key_named(
                "fk_work_task_parent_project_owner",
                [
                    yang_base::field!("work_task.parent_task"),
                    yang_base::field!("work_task.project_project"),
                    yang_base::field!("work_task.owner_user"),
                ],
                [
                    yang_base::field!("work_task.id"),
                    yang_base::field!("work_task.project_project"),
                    yang_base::field!("work_task.owner_user"),
                ],
            )
    }
}

pub(super) fn build_module() -> Result<ModuleSpec, BaseError> {
    let mut module = WorkTaskModule.into_spec().presentation(
        ModulePresentationSpec::new(crate::modules::user_identity(), "任务规划", "account_tree")
            .description("在树形大纲与分页清单中维护个人任务")
            .order(50),
    );
    for view in view::build_all()? {
        module = module.view(view);
    }
    let module = module
        .native_action(actions::TaskOptionsAction::new()?)
        .native_action(actions::CompleteTasksAction);
    module.crud_at_with_mutations(
        "/api/v1/work/tasks",
        actions::AddTaskAction,
        actions::PutTaskAction,
        DelAction::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::definition::ActionPlacement;

    #[test]
    fn task_contract_contains_two_views_tree_relations_and_bulk_action() {
        let module = build_module().unwrap_or_else(|error| panic!("任务 Module 应构建: {error}"));
        assert_eq!(module.views.len(), 2);
        let outline = module
            .views
            .iter()
            .find(|view| view.name.as_str() == "outline")
            .unwrap_or_else(|| panic!("应存在任务树 View"));
        assert_eq!(
            outline.tree.as_ref().and_then(|tree| tree.max_nodes),
            Some(100)
        );
        let complete = outline
            .action_presentations
            .get(&yang_base::action!("work.task.complete"))
            .unwrap_or_else(|| panic!("任务树应声明批量完成"));
        assert_eq!(complete.placement, ActionPlacement::Bulk);

        let table = module
            .table
            .unwrap_or_else(|| panic!("任务 Module 应包含表定义"));
        let project = table
            .fields
            .iter()
            .find(|field| field.name.as_str() == "project_project")
            .unwrap_or_else(|| panic!("应存在项目关系"));
        assert_eq!(
            project
                .relation
                .as_ref()
                .map(|relation| relation.to_string())
                .as_deref(),
            Some("work_project.id")
        );
        let parent = table
            .fields
            .iter()
            .find(|field| field.name.as_str() == "parent_task")
            .unwrap_or_else(|| panic!("应存在父任务关系"));
        assert_eq!(
            parent
                .relation
                .as_ref()
                .map(|relation| relation.to_string())
                .as_deref(),
            Some("work_task.id")
        );
    }
}
