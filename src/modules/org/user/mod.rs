//! `org.user` Module：企业成员关系定义。

mod view;

use yang_base::definition::{
    Fields, Module, ModuleName, ModuleSpec, Radio, Table, TableName, TableSpec, Timestamp,
};
use yang_base::BaseError;

pub(super) const ORG_ID: &str = "org_org";
pub(super) const USER_ID: &str = "user_user";
pub(super) const STATUS: &str = "status";
pub(super) const ACTIVE_STATUS: &str = "active";

struct OrgUserModule;

impl Module for OrgUserModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("org.user")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("org_user"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => yang_base::definition::Key::new().title("ID"),
            org_org => Table::new()
                .title("归属企业")
                .require(true)
                .target(yang_base::field!("org_org.id"))
                .display([yang_base::field!("org_org.name")])
                .select(yang_base::action!("org.org.select"))
                .tenant_key(true),
            user_user => Table::new()
                .title("用户")
                .require(true)
                .target(yang_base::field!("users.id"))
                .display([yang_base::field!("users.username")]),
            status => Radio::<String>::new()
                .title("成员状态")
                .require(true)
                .options([(ACTIVE_STATUS, "启用"), ("disabled", "停用")])
                .default(ACTIVE_STATUS),
            created_at => Timestamp::new().title("创建时间").created_at(),
        }
    }

    fn configure_table(&self, table: TableSpec) -> TableSpec {
        table
            .unique_named(
                "uk_org_user_membership",
                [
                    yang_base::field!("org_user.org_org"),
                    yang_base::field!("org_user.user_user"),
                ],
            )
            .index_named(
                "idx_org_user_user_status",
                [
                    yang_base::field!("org_user.user_user"),
                    yang_base::field!("org_user.status"),
                ],
            )
    }
}

/// 构建成员 Module，并由框架统一生成标准 CRUD Action。
pub(super) fn build_module() -> Result<ModuleSpec, BaseError> {
    OrgUserModule.into_spec().view(view::build()?).crud()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_schema_declares_status_and_database_invariants() {
        let module = OrgUserModule.into_spec();
        let table = module
            .table
            .unwrap_or_else(|| panic!("成员 Module 应包含表定义"));

        assert!(table
            .fields
            .iter()
            .any(|field| field.name.as_str() == STATUS));
        assert!(table.indexes.iter().any(|index| {
            index.unique
                && index.name.as_deref() == Some("uk_org_user_membership")
                && index
                    .fields
                    .iter()
                    .map(|field| field.field().as_str())
                    .eq([ORG_ID, USER_ID])
        }));
        assert!(table.indexes.iter().any(|index| {
            !index.unique && index.name.as_deref() == Some("idx_org_user_user_status")
        }));
    }
}
