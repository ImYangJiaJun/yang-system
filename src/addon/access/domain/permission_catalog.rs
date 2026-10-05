//! 权限目录：从冻结 Catalog 投影全部 Module/Action 声明的权限集合（决策 D3）。
//!
//! Catalog 是权限字符串的唯一事实来源；本模块把它投影为稳定排序的目录，
//! 组合根在 `AppBuilder` 冻结后安装一次，之后运行期只读。

use super::sensitive_permissions::{admin_equivalent_entry, is_admin_equivalent};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use yang_base::definition::AddonSpec;
use yang_base::BaseError;

/// 保留权限键：授权判定下沉到 handler 内实现后（`access.groups` 域），这些键不再由
/// 任何 Action 以 `.permissions(...)` 声明，但仍是**可授予**的目录条目——授予闸门
/// `ensure_declared`、claims 投影与 handler 内的 `has_permission` 判定都依赖目录存续。
/// 投影时以模块名作为声明者，与 `module.default_permissions` 的口径一致。
/// 新增保留键必须同时有对应的 handler 内判定消费它，防止「目录里有、判定不认」。
pub(crate) const RETAINED_PERMISSION_KEYS: &[&str] = &["access.groups.read", "access.groups.write"];

/// 权限字符串格式：点分隔的小写段（如 `access.grants.read`），至少两段。
pub(crate) const PERMISSION_PATTERN: &str = r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$";
/// 权限字符串的最大存储长度。
pub(crate) const PERMISSION_MAX_LENGTH: usize = 128;

/// 权限目录中的一个条目：权限字符串、声明它的操作 ID 列表，以及危害面标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct PermissionEntry {
    pub(crate) permission: String,
    pub(crate) declared_by: Vec<String>,
    /// 该权限是否为「管理员等价权限」（G2）。
    ///
    /// 目录本身推不出这一点（它只记录「有哪些权限」），标记由代码侧的显式清单
    /// `sensitive_permissions` 给出。随条目一起序列化到目录读接口，前端据此把危害面
    /// 显示出来——「可配置的前提是每个权限的危害面可见」。
    pub(crate) admin_equivalent: bool,
    /// 管理员等价的理由（来自代码侧清单，G2）；非管理员等价权限为 `None`。
    ///
    /// 与 `admin_equivalent` 同源同构：清单内权限必有理由（`sensitive_permissions`
    /// 的单测钉住「每条都带非空理由」），清单外恒为 `None`。序列化字段名 `reason`，
    /// 可空——前端在展示危害面时把理由一并显示出来。
    pub(crate) reason: Option<String>,
}

impl PermissionEntry {
    pub(crate) fn permission(&self) -> &str {
        &self.permission
    }

    #[cfg(test)]
    pub(crate) fn declared_by(&self) -> &[String] {
        &self.declared_by
    }

    #[cfg(test)]
    pub(crate) fn admin_equivalent(&self) -> bool {
        self.admin_equivalent
    }

    #[cfg(test)]
    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
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
    // 保留键补投影（见 [`RETAINED_PERMISSION_KEYS`]）：它们不来自任何 Action 声明，
    // 直接并入目录，保证「授予闸门」与「handler 内判定」两侧看到同一份事实。
    for retained in RETAINED_PERMISSION_KEYS {
        declared
            .entry(retained.to_string())
            .or_default()
            .push("access.groups".to_string());
    }
    declared
        .into_iter()
        .map(|(permission, mut declared_by)| {
            declared_by.sort();
            declared_by.dedup();
            let admin_equivalent = is_admin_equivalent(&permission);
            let reason = admin_equivalent_entry(&permission).map(|entry| entry.reason.to_string());
            PermissionEntry {
                permission,
                declared_by,
                admin_equivalent,
                reason,
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
                // 按字典序：grants < groups——保留键恒定在投影里（handler 内判定
                // 依赖它们可授予、可入 claims），与 Action 声明的键同场排序。
                "access.grants.read",
                "access.grants.write",
                "access.groups.read",
                "access.groups.write",
                "account.user.me",
                "account.user.session",
            ]
        );
        assert_eq!(
            entries[0].declared_by(),
            ["access.grants.list_permissions", "account.user.me"]
        );
        assert_eq!(entries[1].declared_by(), ["access.grants.grant_permission"]);
        assert_eq!(entries[5].declared_by(), ["account.user"]);
        // 保留键的声明者是模块名（无 Action 声明它们，见 RETAINED_PERMISSION_KEYS）。
        assert_eq!(entries[2].declared_by(), ["access.groups"]);
        assert_eq!(entries[3].declared_by(), ["access.groups"]);
    }

    #[test]
    fn projection_of_catalog_without_permissions_keeps_only_retained_keys() {
        let addons = vec![addon(
            "account",
            vec![module("account.user", &[], &[("me", &[][..])])],
        )];

        // 没有 Action 声明权限时投影只剩保留键：目录的「空」不再是真空，
        // 因为 `access.groups.read/write` 必须始终可授予（见 RETAINED_PERMISSION_KEYS）。
        let entries = project_permissions(&addons);
        let permissions: Vec<&str> = entries.iter().map(PermissionEntry::permission).collect();
        assert_eq!(permissions, ["access.groups.read", "access.groups.write"]);
        // 保留键的声明者是模块名（与 module.default_permissions 的口径一致）。
        for entry in entries {
            assert_eq!(entry.declared_by(), ["access.groups"]);
        }
    }

    #[test]
    fn projection_marks_admin_equivalent_permissions_from_the_explicit_list() {
        // 标记来自代码侧清单（`sensitive_permissions`），不是 Catalog 能推出的：
        // 同一份投影里，清单内的权限必须为 true、清单外必须为 false。
        let addons = vec![addon(
            "account",
            vec![module(
                "account.user",
                &["account.users.manage"],
                &[(
                    "admin_issue_password_reset",
                    &["account.users.reset_credentials"][..],
                )],
            )],
        )];

        let entries = project_permissions(&addons);
        let flag = |permission: &str| {
            entries
                .iter()
                .find(|entry| entry.permission() == permission)
                .unwrap_or_else(|| panic!("投影应包含 {permission}"))
                .admin_equivalent()
        };
        assert!(flag("account.users.reset_credentials"), "清单内必须标记");
        assert!(!flag("account.users.manage"), "清单外不得标记");

        // reason 与 admin_equivalent 同源：清单内权限带清单理由，清单外恒为 None。
        let reason = |permission: &str| {
            entries
                .iter()
                .find(|entry| entry.permission() == permission)
                .unwrap_or_else(|| panic!("投影应包含 {permission}"))
                .reason()
        };
        let listed_reason = reason("account.users.reset_credentials")
            .unwrap_or_else(|| panic!("清单内权限必须带理由"));
        assert!(
            listed_reason.contains("重置凭证"),
            "理由必须是清单里的中文危害面说明，实际: {listed_reason}"
        );
        assert_eq!(reason("account.users.manage"), None, "清单外权限不得带理由");
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
                admin_equivalent: false,
                reason: None,
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
