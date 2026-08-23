//! `work.project` Module：个人项目组合与关系选项。

mod actions;
mod domain;

use yang_base::definition::{
    Fields, Module, ModuleName, ModulePresentationSpec, ModuleSpec, Radio, Str, Table, TableName,
    TableSpec, Timestamp,
};
use yang_base::BaseError;

struct WorkProjectModule;

impl Module for WorkProjectModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("work.project")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("work_project"))
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
            name => Str::new()
                .title("项目名称")
                .require(true)
                .max_length(120)
                .searchable(true)
                .sortable(true),
            status => Radio::<String>::new()
                .title("项目状态")
                .require(true)
                .options([("active", "进行中"), ("archived", "已归档")])
                .default("active")
                .filterable(true)
                .sortable(true),
            created_at => Timestamp::new().title("创建时间").created_at().sortable(true),
            updated_at => Timestamp::new().title("更新时间").updated_at().sortable(true),
        }
    }

    fn configure_table(&self, table: TableSpec) -> TableSpec {
        table
            .unique_named(
                "uk_work_project_owner_name",
                [
                    yang_base::field!("work_project.owner_user"),
                    yang_base::field!("work_project.name"),
                ],
            )
            .unique_named(
                "uk_work_project_id_owner",
                [
                    yang_base::field!("work_project.id"),
                    yang_base::field!("work_project.owner_user"),
                ],
            )
            .index_named(
                "idx_work_project_owner_status_name",
                [
                    yang_base::field!("work_project.owner_user"),
                    yang_base::field!("work_project.status"),
                    yang_base::field!("work_project.name"),
                ],
            )
            .foreign_key_named(
                "fk_work_project_owner",
                [yang_base::field!("work_project.owner_user")],
                [yang_base::field!("users.id")],
            )
    }
}

pub(super) fn build_module() -> Result<ModuleSpec, BaseError> {
    let module = WorkProjectModule
        .into_spec()
        .presentation(
            ModulePresentationSpec::new(crate::addon::user_identity(), "项目组合", "workspaces")
                .description("维护个人项目，并作为任务关系选择的数据源")
                .order(40),
        )
        .view(domain::build_portfolio_view()?);
    actions::register_all(module)?.crud_at("/api/v1/work/projects")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_schema_has_tenant_and_relation_indexes() {
        let table = WorkProjectModule
            .into_spec()
            .table
            .unwrap_or_else(|| panic!("项目 Module 应包含表定义"));
        let owner = table
            .fields
            .iter()
            .find(|field| field.name.as_str() == "owner_user")
            .unwrap_or_else(|| panic!("项目表应声明 owner_user"));
        assert!(owner.tenant_key);
        assert!(table.indexes.iter().any(|index| {
            index.unique && index.name.as_deref() == Some("uk_work_project_owner_name")
        }));
    }
}
