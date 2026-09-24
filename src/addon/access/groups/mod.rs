//! `access.groups` Module（module 层）：权限组管理装配。
//!
//! 本文件就是这个模块的"定义卡"：表、上下文、中间件、Action 注册表、
//! Step-up 守卫与展示投影按分区顺序装配；业务用例全部在 `actions/` 的
//! 自包含文件中。

mod actions;
pub(super) mod table;

use super::domain::context::Access;
use super::domain::permission_catalog::PermissionCatalogHandle;
use crate::addon::account::user_from_claims;
use crate::authorization::{
    AuthorizationPort, AuthorizationVersionValidator, RequestFingerprintResolver, StepUpServices,
};
use std::sync::Arc;
use yang_base::action::TokenAuthMiddleware;
use yang_base::definition::{ModuleName, ModuleSpec};
use yang_base::BaseError;

/// 装配 `access.groups` Module：表 → 中间件 → Action 注册表 → Step-up 守卫。
pub(super) fn build_module(
    authorization_validator: AuthorizationVersionValidator,
    step_up: Option<StepUpServices>,
    _permission_catalog: PermissionCatalogHandle,
    _authorization: AuthorizationPort,
    access: Arc<Access>,
) -> Result<ModuleSpec, BaseError> {
    let table = table::groups_table_spec()?;
    let mut module = ModuleSpec::new(
        ModuleName::new("access.groups").map_err(|e| BaseError::ConfigError(e.to_string()))?,
    )
    .table(table)
    .middleware(
        TokenAuthMiddleware::new(user_from_claims)
            .with_claims_validator(authorization_validator)
            .authenticate_public_actions(),
    );
    module = actions::register_all(module, Arc::clone(&access));
    if let Some(step_up) = step_up {
        for target in step_up_targets() {
            module = module.middleware(
                step_up.middleware(target, RequestFingerprintResolver::global("access-groups")),
            );
        }
    }
    Ok(module)
}

/// 需要 Step-up 重认证的组写操作：组生命周期、组条目、组成员都直接改变授权事实。
///
/// 本清单必须与冻结 Catalog 里 `access.groups` 的全部非只读 Action 逐项相等；
/// 该等式的守护测试见下方 `every_group_mutation_is_step_up_protected`。
fn step_up_targets() -> Vec<yang_base::definition::ActionRef> {
    vec![
        yang_base::action!("access.groups.create_group"),
        yang_base::action!("access.groups.update_group"),
        yang_base::action!("access.groups.delete_group"),
        yang_base::action!("access.groups.add_group_item"),
        yang_base::action!("access.groups.remove_group_item"),
        yang_base::action!("access.groups.add_group_member"),
        yang_base::action!("access.groups.remove_group_member"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecuritySettings;
    use std::collections::BTreeSet;
    use yang_base::definition::{ActionName, ActionRef, ActionSpec, BuiltApp, ModuleName};
    use yang_base::tools::ToolsBuilder;

    /// 被测 Module 的全限定名。
    const GROUPS_MODULE: &str = "access.groups";

    /// 只读权限后缀：声明的权限全都是 `.read` 才说明这个 Action 不改变授权事实。
    const READ_PERMISSION_SUFFIX: &str = ".read";

    /// 只读 Action 的判据必须能从 Catalog 自动得出，否则「新增 Action 忘了挂
    /// Step-up」就永远测不出来。这里取 fail-closed 的反向定义：**只有**当 Action
    /// 声明了非空且全部以 `.read` 结尾的权限集合时才算只读；权限为空（公开或未声明）
    /// 或含非 `.read` 权限的一律按授权事实变更处理。
    fn is_read_only(action: &ActionSpec) -> bool {
        !action.permissions.is_empty()
            && action
                .permissions
                .iter()
                .all(|permission| permission.ends_with(READ_PERMISSION_SUFFIX))
    }

    /// 构建带 Step-up 的冻结 Catalog：元数据导出路径复用与运行时同源的组合根
    /// （`build_metadata_app` 以 `Some(step_up)` 装配），因此这里的 Catalog 与
    /// 生产启动时冻结的那一份逐字段一致，不连接任何外部依赖。
    fn frozen_catalog() -> BuiltApp {
        let security = Arc::new(SecuritySettings {
            argon2_max_concurrency: 1,
            auth_rate_limit_window_seconds: 60,
            auth_rate_limit_ip_attempts: 30,
            auth_rate_limit_username_attempts: 30,
            password_reset_ttl_seconds: 900,
            issue_refresh_credential_version: true,
            trusted_proxy_cidrs: Vec::new(),
            totp: None,
        });
        let tools = Arc::new(
            ToolsBuilder::new()
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        crate::app::build_metadata_app(tools, security)
            .unwrap_or_else(|error| panic!("元数据应用应构建成功: {error:#}"))
            .runtime
    }

    /// 从冻结 Catalog 枚举 `access.groups` 的全部非只读 Action，逐个折算为 ActionRef。
    fn catalog_mutation_targets(app: &BuiltApp) -> Vec<ActionRef> {
        let module = app
            .catalog()
            .addons()
            .iter()
            .flat_map(|addon| &addon.modules)
            .find(|module| module.name.as_str() == GROUPS_MODULE)
            .unwrap_or_else(|| panic!("冻结 Catalog 必须包含 {GROUPS_MODULE} 模块"));
        let module_name = ModuleName::new(GROUPS_MODULE)
            .unwrap_or_else(|error| panic!("{GROUPS_MODULE} 应是合法模块名: {error}"));
        module
            .actions()
            .iter()
            .filter(|action| !is_read_only(action))
            .map(|action| {
                let action_name = ActionName::new(action.name.as_str())
                    .unwrap_or_else(|error| panic!("Action {} 应是合法名称: {error}", action.name));
                ActionRef::new(module_name.clone(), action_name)
            })
            .collect()
    }

    /// 守卫：Step-up 登记必须与冻结 Catalog 里的组写 Action 集合逐项相等。
    ///
    /// 判据完全来自 Catalog（非只读 = 需要重认证），因此「新增一个组管理 Action 却
    /// 忘了登记 Step-up」必然让本测试变红；反过来，登记一个 Catalog 里不存在的
    /// Action（拼写错误）同样变红。
    #[test]
    fn every_group_mutation_is_step_up_protected() {
        let app = frozen_catalog();
        let expected: BTreeSet<ActionRef> = catalog_mutation_targets(&app).into_iter().collect();
        assert!(
            expected.len() >= 7,
            "{GROUPS_MODULE} 的写 Action 至少应有 7 个（建/改/删组 + 加/移除条目 + 加/移出成员），\
             实际 {}——判据或 Catalog 被削弱了",
            expected.len()
        );

        let actual: BTreeSet<ActionRef> = step_up_targets().into_iter().collect();
        assert_eq!(
            actual, expected,
            "Step-up 登记与冻结 Catalog 的写 Action 集合必须逐项相等：漏登记 = \
             无重认证的授权变更入口"
        );
    }
}
