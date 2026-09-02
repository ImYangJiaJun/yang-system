//! 权限目录：从冻结 Catalog 投影全部 Module/Action 声明的权限集合（决策 D3）。
//!
//! Catalog 是权限字符串的唯一事实来源；本模块把它投影为稳定排序的目录，
//! 组合根在 `AppBuilder` 冻结后安装一次，之后运行期只读。

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

/// 权限字符串格式：点分隔的小写段（如 `access.grants.read`），至少两段。
pub(crate) const PERMISSION_PATTERN: &str = r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$";
/// 权限字符串的最大存储长度。
pub(crate) const PERMISSION_MAX_LENGTH: usize = 128;

/// 权限目录中的一个条目：权限字符串与声明它的操作 ID 列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct PermissionEntry {
    permission: String,
    declared_by: Vec<String>,
}

impl PermissionEntry {
    pub(crate) fn permission(&self) -> &str {
        &self.permission
    }

    #[cfg(test)]
    pub(crate) fn declared_by(&self) -> &[String] {
        &self.declared_by
    }
}

/// 从 Catalog 的 Addon 定义投影权限目录。
///
/// Module 默认权限与 Action 权限取并集；条目按权限字符串稳定排序，
/// 声明者按操作 ID 稳定排序并去重，保证同一 Catalog 的投影结果确定。
pub(crate) fn project_permissions(addons: &[AddonSpec]) -> Vec<PermissionEntry> {
    let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for addon in addons {
        for module in &addon.modules {
            for permission in &module.default_permissions {
                declared
                    .entry(permission.clone())
                    .or_default()
                    .push(module.name.as_str().to_string());
            }
            for action in module.actions() {
                let operation_id = format!("{}.{}", module.name.as_str(), action.name.as_str());
                for permission in &action.permissions {
                    declared
                        .entry(permission.clone())
                        .or_default()
                        .push(operation_id.clone());
                }
            }
        }
    }
    declared
        .into_iter()
        .map(|(permission, mut declared_by)| {
            declared_by.sort();
            declared_by.dedup();
            PermissionEntry {
                permission,
                declared_by,
            }
        })
        .collect()
}

/// 运行期权限目录句柄：组合根在 Catalog 冻结后安装一次，之后只读。
///
/// 句柄经 `Access` 上下文显式持有，不是进程级全局单例。
#[derive(Clone, Default)]
pub(crate) struct PermissionCatalogHandle {
    projection: Arc<OnceLock<Vec<PermissionEntry>>>,
}

impl PermissionCatalogHandle {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 安装冻结 Catalog 的投影；重复安装说明组合根装配错误，fail-closed。
    pub(crate) fn install(&self, entries: Vec<PermissionEntry>) -> Result<(), BaseError> {
        self.projection
            .set(entries)
            .map_err(|_| BaseError::ConfigError("权限目录投影已安装，禁止重复安装".to_string()))
    }

    /// 读取已安装的目录；未安装时 fail-closed（Schema-only 应用不服务权限查询）。
    pub(crate) fn entries(&self) -> Result<&[PermissionEntry], BaseError> {
        self.projection
            .get()
            .map(Vec::as_slice)
            .ok_or_else(|| BaseError::ConfigError("权限目录投影尚未安装".to_string()))
    }

    /// 权限必须存在于目录中；未声明的权限不能被授予，fail-closed。
    pub(crate) fn ensure_declared(&self, permission: &str) -> Result<(), BaseError> {
        if self
            .entries()?
            .iter()
            .any(|entry| entry.permission() == permission)
        {
            return Ok(());
        }
        Err(BaseError::ParamInvalid(
            "permission".to_string(),
            "权限未在任何 Action 上声明，不能授予".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yang_base::action::ActionContext;
    use yang_base::definition::{
        ActionName, AddonName, ModuleName, ModuleSpec, ParamInput, Params,
    };

    #[derive(Debug, serde::Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct TestInput {}

    impl ParamInput for TestInput {
        fn params() -> Params {
            Params::new()
        }
    }

    async fn noop(_ctx: ActionContext, _input: TestInput) -> Result<serde_json::Value, BaseError> {
        Ok(serde_json::Value::Null)
    }

    fn module(name: &str, default_permissions: &[&str], actions: &[(&str, &[&str])]) -> ModuleSpec {
        let mut spec = ModuleSpec::new(
            ModuleName::new(name).unwrap_or_else(|error| panic!("Module 名应有效: {error}")),
        );
        spec.default_permissions = default_permissions
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        for (action_name, permissions) in actions {
            spec = spec
                .action_fn(
                    ActionName::new(*action_name)
                        .unwrap_or_else(|error| panic!("Action 名应有效: {error}")),
                    noop,
                )
                .permissions(permissions.iter().copied())
                .register();
        }
        spec
    }

    fn addon(name: &str, modules: Vec<ModuleSpec>) -> AddonSpec {
        let mut spec = AddonSpec::new(
            AddonName::new(name).unwrap_or_else(|error| panic!("Addon 名应有效: {error}")),
        );
        spec.modules = modules;
        spec
    }

    #[test]
    fn projection_merges_action_and_module_permissions_in_stable_order() {
        let addons = vec![
            addon(
                "access",
                vec![module(
                    "access.grants",
                    &[],
                    &[
                        ("grant_permission", &["access.grants.write"][..]),
                        ("list_permissions", &["access.grants.read"][..]),
                    ],
                )],
            ),
            addon(
                "account",
                vec![module(
                    "account.user",
                    &["account.user.session"],
                    &[("me", &["access.grants.read", "account.user.me"][..])],
                )],
            ),
        ];

        let entries = project_permissions(&addons);

        let permissions: Vec<&str> = entries.iter().map(PermissionEntry::permission).collect();
        assert_eq!(
            permissions,
            [
                "access.grants.read",
                "access.grants.write",
                "account.user.me",
                "account.user.session",
            ]
        );
        assert_eq!(
            entries[0].declared_by(),
            ["access.grants.list_permissions", "account.user.me"]
        );
        assert_eq!(entries[1].declared_by(), ["access.grants.grant_permission"]);
        assert_eq!(entries[3].declared_by(), ["account.user"]);
    }

    #[test]
    fn projection_of_catalog_without_permissions_is_empty() {
        let addons = vec![addon(
            "account",
            vec![module("account.user", &[], &[("me", &[][..])])],
        )];

        assert!(project_permissions(&addons).is_empty());
    }

    #[test]
    fn handle_is_fail_closed_before_and_after_install() {
        let handle = PermissionCatalogHandle::new();
        assert!(matches!(handle.entries(), Err(BaseError::ConfigError(_))));
        assert!(matches!(
            handle.ensure_declared("access.grants.read"),
            Err(BaseError::ConfigError(_))
        ));

        handle
            .install(vec![PermissionEntry {
                permission: "access.grants.read".to_string(),
                declared_by: vec!["access.grants.list_permissions".to_string()],
            }])
            .unwrap_or_else(|error| panic!("首次安装应成功: {error}"));
        let entries = handle
            .entries()
            .unwrap_or_else(|error| panic!("安装后应可读取目录: {error}"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].permission(), "access.grants.read");
        assert!(handle.ensure_declared("access.grants.read").is_ok());
        assert!(matches!(
            handle.ensure_declared("access.grants.write"),
            Err(BaseError::ParamInvalid(field, _)) if field == "permission"
        ));
        assert!(matches!(
            handle.install(Vec::new()),
            Err(BaseError::ConfigError(_))
        ));
    }
}
