//! `org.org` Module：企业主数据定义。

mod actions;

use yang_base::definition::{
    Actions, Fields, Module, ModuleName, ModuleSpec, Str, TableName, Timestamp,
};

struct OrganizationModule;

impl Module for OrganizationModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("org.org")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("org_org"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => yang_base::definition::Key::new().title("ID"),
            name => Str::new()
                .title("企业名称")
                .require(true)
                .max_length(100)
                .searchable(true)
                .sortable(true),
            code => Str::new()
                .title("企业编号")
                .require(true)
                .max_length(32)
                .unique(true)
                .searchable(true),
            status => Str::new().title("状态").require(true).max_length(16),
            created_at => Timestamp::new().title("创建时间").created_at(),
        }
    }

    fn actions(&self) -> Actions {
        actions::all()
    }
}

/// 将企业 Module 的 Schema 与 Action 原子聚合为 `ModuleSpec`。
pub(super) fn build_module() -> ModuleSpec {
    OrganizationModule.into_spec()
}
