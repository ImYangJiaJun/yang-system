//! `org.org` Module：企业主数据定义。

mod actions;
mod model;
mod query;

use yang_base::definition::{
    Actions, Fields, Module, ModuleName, ModulePresentationSpec, ModuleSpec, Radio, Str, TableName,
    Timestamp,
};

pub(super) const STATUS: &str = "status";
pub(super) const ACTIVE_STATUS: &str = "active";

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
            id => yang_base::definition::Key::new().title("ID").filterable(true),
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
            status => Radio::<String>::new()
                .title("状态")
                .require(true)
                .options([(ACTIVE_STATUS, "启用"), ("disabled", "停用")])
                .default(ACTIVE_STATUS)
                .filterable(true),
            created_at => Timestamp::new()
                .title("创建时间")
                .created_at()
                .sortable(true),
        }
    }

    fn actions(&self) -> Actions {
        actions::all()
    }
}

/// 将企业 Module 的 Schema 与 Action 原子聚合为 `ModuleSpec`。
pub(super) fn build_module() -> ModuleSpec {
    OrganizationModule.into_spec().presentation(
        ModulePresentationSpec::new(
            crate::modules::organization_identity(),
            "企业资料",
            "organization_profile",
        )
        .description("查看当前企业主数据")
        .order(20)
        .primary_action(yang_base::action!("org.org.list")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_contract_fields_are_explicitly_authorized() {
        let table = OrganizationModule
            .into_spec()
            .table
            .unwrap_or_else(|| panic!("企业 Module 应包含表定义"));
        for name in ["id", STATUS] {
            let field = table
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("应存在字段 {name}"));
            assert!(field.access.filterable, "resolver 查询键 {name} 必须可筛选");
        }
        for name in ["name", "created_at"] {
            let field = table
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("应存在字段 {name}"));
            assert!(field.access.sortable, "Action 排序字段 {name} 必须显式授权");
        }
    }

    #[test]
    fn organization_remains_read_only_until_disable_state_machine_is_implemented() {
        let module = OrganizationModule.into_spec();
        let actions = module
            .actions()
            .iter()
            .map(|action| (action.name.as_str(), action.route.method.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            actions,
            [("list", "GET"), ("select", "POST")],
            "企业模块在 active -> disabling -> disabled 状态机落地前不得暴露写 Action"
        );
    }
}
