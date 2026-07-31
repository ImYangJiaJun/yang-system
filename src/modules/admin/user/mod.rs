//! `admin.user` Module：平台账号定义。

mod actions;
mod model;
mod repository;
mod service;

use crate::config::SecuritySettings;
use crate::modules::account::AuthRateLimiter;
use repository::AdminRepository;
use service::AdminService;
use std::sync::Arc;
use yang_base::definition::{
    ActionInteraction, ActionPlacement, ActionPresentationSpec, ActionRef, Fields, Module,
    ModuleName, ModulePresentationSpec, ModuleSpec, Radio, Str, Switch, Table, TableName,
    Timestamp,
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

pub(super) fn step_up_targets() -> [ActionRef; 3] {
    [
        yang_base::action!("admin.user.add"),
        yang_base::action!("admin.user.set_status"),
        yang_base::action!("admin.user.set_admin"),
    ]
}

/// 构建平台账号 Module。
pub(super) fn build_module(security: &SecuritySettings) -> Result<ModuleSpec, BaseError> {
    let module = AdminUserModule.into_spec();
    let table = module
        .table
        .as_ref()
        .ok_or(BaseError::TableDefinitionNotSet)?
        .table_definition()?;
    let password_reset_enabled = security.issue_refresh_credential_version;
    let service = Arc::new(AdminService::new(
        AdminRepository::new(table),
        Arc::new(AuthRateLimiter::new(security)),
        security.password_reset_ttl_seconds,
        password_reset_enabled,
    ));
    Ok(
        actions::register_all(module, service, password_reset_enabled).presentation(
            ModulePresentationSpec::new(
                crate::modules::presentation::administrator_identity(),
                "平台账号",
                "admin_users",
            )
            .description("查询并维护平台管理账号")
            .order(10)
            .primary_action(yang_base::action!("admin.user.list"))
            .present_action(
                yang_base::action!("admin.user.add"),
                ActionPresentationSpec::new(ActionPlacement::Toolbar, ActionInteraction::Form),
            ),
        ),
    )
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

    #[test]
    fn every_platform_privilege_mutation_is_explicitly_step_up_protected() {
        let protected = step_up_targets()
            .into_iter()
            .map(|target| target.action().as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            protected,
            ["add", "set_admin", "set_status"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }
}
