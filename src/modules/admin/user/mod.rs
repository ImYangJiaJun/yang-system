//! `admin.user` Module：平台账号定义。

mod actions;
mod repository;
mod service;

use repository::AdminRepository;
use service::AdminService;
use std::sync::Arc;
use yang_base::definition::{
    Fields, Module, ModuleName, ModuleSpec, Radio, Str, Switch, Table, TableName, Timestamp,
};
use yang_base::BaseError;

pub(super) const USER_ID: &str = "user_user";
pub(super) const NAME: &str = "name";
pub(super) const POSITION: &str = "position";
pub(super) const STATUS: &str = "status";
pub(super) const IS_ADMIN: &str = "admin";
pub(super) const BOOTSTRAP_KEY: &str = "bootstrap_key";
pub(super) const ACTIVE_STATUS: &str = "active";
pub(super) const SYSTEM_ROLE: &str = "system";

struct AdminUserModule;

impl Module for AdminUserModule {
    fn name(&self) -> ModuleName {
        yang_base::module!("admin.user")
    }

    fn table(&self) -> Option<TableName> {
        Some(yang_base::table!("admin_user"))
    }

    fn fields(&self) -> Fields {
        yang_base::fields! {
            id => yang_base::definition::Key::new().title("ID").filterable(true),
            user_user => Table::new()
                .title("用户账号")
                .require(true)
                .target(yang_base::field!("users.id"))
                .display([yang_base::field!("users.username")])
                .unique(true)
                .filterable(true),
            name => Str::new()
                .title("姓名")
                .require(true)
                .max_length(50)
                .searchable(true)
                .sortable(true),
            position => Str::new().title("职务").max_length(50),
            status => Radio::<String>::new()
                .title("状态")
                .require(true)
                .options([(ACTIVE_STATUS, "启用"), ("disabled", "停用")])
                .default(ACTIVE_STATUS)
                .filterable(true),
            admin => Switch::new()
                .title("超级管理员")
                .require(true)
                .default(false)
                .filterable(true),
            bootstrap_key => Str::new()
                .title("初始化占位")
                .max_length(32)
                .unique(true)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
            created_at => Timestamp::new().title("创建时间").created_at(),
            updated_at => Timestamp::new().title("更新时间").updated_at(),
        }
    }
}

/// 构建平台账号 Module。
pub(super) fn build_module() -> Result<ModuleSpec, BaseError> {
    let module = AdminUserModule.into_spec();
    let table = module
        .table
        .as_ref()
        .ok_or(BaseError::TableDefinitionNotSet)?
        .table_definition()?;
    let service = Arc::new(AdminService::new(AdminRepository::new(table)));
    Ok(actions::register_all(module, service))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_schema_keeps_identity_and_bootstrap_invariants_in_database() {
        let table = AdminUserModule
            .into_spec()
            .table
            .unwrap_or_else(|| panic!("平台账号 Module 应包含表定义"));

        assert_eq!(table.name.as_str(), "admin_user");
        assert!(table
            .fields
            .iter()
            .any(|field| { field.name.as_str() == USER_ID && field.storage.unique }));
        assert!(table.fields.iter().any(|field| {
            field.name.as_str() == BOOTSTRAP_KEY && field.storage.unique && field.access.secret
        }));
        for name in [USER_ID, STATUS, IS_ADMIN] {
            let field = table
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("应存在字段 {name}"));
            assert!(field.access.filterable, "管理查询字段 {name} 必须可筛选");
        }
    }
}
