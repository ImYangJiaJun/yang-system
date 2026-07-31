//! `org.user` Module：企业成员关系定义。

mod actions;
mod guard;
mod repository;
mod view;

use yang_base::definition::{
    ActionRef, Fields, Module, ModuleName, ModulePresentationSpec, ModuleSpec, Radio, Str, Switch,
    Table, TableName, TableSpec, Timestamp,
};
use yang_base::BaseError;

pub(super) const ORG_ID: &str = "org_org";
pub(super) const USER_ID: &str = "user_user";
pub(super) const NAME: &str = "name";
pub(super) const IS_ADMIN: &str = "admin";
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
                .tenant_key(true)
                .filterable(true),
            user_user => Table::new()
                .title("用户")
                .require(true)
                .target(yang_base::field!("users.id"))
                .display([yang_base::field!("users.username")])
                .filterable(true),
            name => Str::new()
                .title("姓名")
                .max_length(50)
                .searchable(true)
                .sortable(true),
            position => Str::new().title("职务").max_length(50),
            email => Str::new().title("邮箱").max_length(254),
            phone => Str::new().title("手机").max_length(20),
            admin => Switch::new()
                .title("企业管理员")
                .require(true)
                .default(false)
                .filterable(true),
            status => Radio::<String>::new()
                .title("成员状态")
                .require(true)
                .options([(ACTIVE_STATUS, "启用"), ("disabled", "停用")])
                .default(ACTIVE_STATUS)
                .filterable(true),
            created_at => Timestamp::new().title("创建时间").created_at(),
            updated_at => Timestamp::new().title("更新时间").updated_at(),
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
                "idx_org_user_user_status_admin",
                [
                    yang_base::field!("org_user.user_user"),
                    yang_base::field!("org_user.status"),
                    yang_base::field!("org_user.admin"),
                ],
            )
    }
}

/// 构建成员 Module，并由框架统一生成标准 CRUD Action。
pub(super) fn build_module() -> Result<ModuleSpec, BaseError> {
    OrgUserModule
        .into_spec()
        .presentation(
            ModulePresentationSpec::new(
                crate::modules::presentation::organization_identity(),
                "企业成员",
                "organization_members",
            )
            .description("查询并维护当前企业成员")
            .order(30),
        )
        .view(view::build()?)
        .crud_at_with_mutations(
            "/api/v1/org/users",
            actions::AddMembershipAction,
            actions::PutMembershipAction,
            actions::DeleteMembershipAction,
        )
}

fn resource_authorizer_targets() -> [ActionRef; 3] {
    [
        yang_base::action!("org.user.add"),
        yang_base::action!("org.user.put"),
        yang_base::action!("org.user.del"),
    ]
}

/// 在认证与租户解析之后，为每个成员 mutation 注册确定目标的资源授权器。
pub(super) fn register_resource_authorizers(mut module: ModuleSpec) -> ModuleSpec {
    for target in resource_authorizer_targets() {
        module = module.middleware(guard::OrgAdminGuardMiddleware::new(target));
    }
    module
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
            !index.unique
                && index.name.as_deref() == Some("idx_org_user_user_status_admin")
                && index
                    .fields
                    .iter()
                    .map(|field| field.field().as_str())
                    .eq([USER_ID, STATUS, IS_ADMIN])
        }));
        for name in [ORG_ID, USER_ID, STATUS, IS_ADMIN] {
            let field = table
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| panic!("应存在字段 {name}"));
            assert!(field.access.filterable, "resolver 查询键 {name} 必须可筛选");
        }
    }

    #[test]
    fn every_member_mutation_has_an_exact_resource_authorizer_target() {
        let module = build_module().unwrap_or_else(|error| panic!("成员模块应可构建: {error}"));
        let mutation_names = module
            .actions()
            .iter()
            // 新 Action 默认按高危 mutation 处理；只有逐项审计过的只读 Action 可进入白名单。
            .filter(|action| !matches!(action.name.as_str(), "get" | "select" | "table"))
            .map(|action| action.name.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let guarded_names = resource_authorizer_targets()
            .into_iter()
            .map(|target| target.action().as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(mutation_names, guarded_names);
    }
}
