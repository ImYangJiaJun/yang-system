//! `org.user` Module：企业成员关系定义。

mod view;

use yang_base::definition::{Fields, Module, ModuleName, ModuleSpec, Table, TableName, Timestamp};
use yang_base::BaseError;

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
            created_at => Timestamp::new().title("创建时间").created_at(),
        }
    }
}

/// 构建成员 Module，并由框架统一生成标准 CRUD Action。
pub(super) fn build_module() -> Result<ModuleSpec, BaseError> {
    OrgUserModule.into_spec().view(view::build()?).crud()
}
