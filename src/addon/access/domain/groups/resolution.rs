//! 有效权限解析：把「用户所在组」折叠为权限集合。
//!
//! 本模块是**唯一的解析实现**——`GroupGrantResolver` 与组管理 Action 的
//! 提权校验都调用它（spec §8.1），避免校验与实际解析漂移成两套语义。
//! 全部为纯函数，不依赖数据库，因此可以无夹具单测。

// 消费者（`GroupGrantResolver` 在 Task 5、组管理 Action 在 Task 10+）晚于本任务
// 接入，与 `repository.rs` / `tables.rs` 同例显式豁免 dead-code 门禁。
#![allow(dead_code)]

use super::repository::SYSTEM_ADMIN_GROUP_KEY;
use crate::addon::access::domain::permission_catalog::PermissionCatalogHandle;
use std::collections::BTreeSet;
use yang_base::BaseError;

/// 读取已安装的权限目录；未安装时透传 ConfigError（fail-closed）。
pub(crate) fn catalog_permissions(
    handle: &PermissionCatalogHandle,
) -> Result<Vec<String>, BaseError> {
    Ok(handle
        .entries()?
        .iter()
        .map(|entry| entry.permission().to_string())
        .collect())
}

/// 语义在**解析期**被特殊解释、因而不可被公开创建路径占用的保留 `group_key` 集合。
///
/// [`resolve_group_permissions`] 判断一个组是否全权**只看 `group_key` 字符串**——
/// 命中 [`SYSTEM_ADMIN_GROUP_KEY`] 即返回**整个权限目录**，与组条目、成员无关。也就是说
/// 这个字符串就是全权组的身份。因此这些 key 不能被公开的建组路径占用：任何持
/// `access.groups.write` 的账号若能写出同名 key，就等于凭空造出一个解析为全权目录的空组
/// （唯一可达状态是内置组不在库中时，见设计 §7.3 的灾备态）。
///
/// 把它们集中在这里，是为了让「保留」这件事有一个可被测试钉住的**集合**，而不是散落在
/// 各 Action 里的字面量比较。新增解析期特殊语义的 key 时必须同时把条目加进来：
/// `create_group.rs` 的单测会逐项校验集合里每个成员都被建组路径拒绝，漏加即失去创建期防线。
pub(crate) const RESERVED_GROUP_KEYS: &[&str] = &[SYSTEM_ADMIN_GROUP_KEY];

/// 该 `group_key` 是否为解析期保留值（见 [`RESERVED_GROUP_KEYS`]）。
pub(crate) fn is_reserved_group_key(group_key: &str) -> bool {
    RESERVED_GROUP_KEYS.contains(&group_key)
}

/// 一个组对有效权限的贡献。
///
/// 内置全权组取整个目录（因此未来新增 Action 的权限自动纳入）；
/// 普通组取其条目与目录的交集——**孤儿条目被静默丢弃**，因为匹配不到
/// 任何 Action 的权限字符串不可能放行任何请求。
pub(crate) fn resolve_group_permissions(
    group_key: &str,
    items: &[String],
    catalog: &[String],
) -> Vec<String> {
    if group_key == SYSTEM_ADMIN_GROUP_KEY {
        let mut all = catalog.to_vec();
        all.sort();
        all.dedup();
        return all;
    }
    let known: BTreeSet<&str> = catalog.iter().map(String::as_str).collect();
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    for item in items {
        if known.contains(item.as_str()) {
            resolved.insert(item.clone());
        }
    }
    resolved.into_iter().collect()
}

/// 组内已不在权限目录中的条目（spec §8.4：只报告，不自动清理）。
pub(crate) fn orphan_items<'a>(items: &'a [String], catalog: &[String]) -> Vec<&'a str> {
    let known: BTreeSet<&str> = catalog.iter().map(String::as_str).collect();
    let mut orphans: Vec<&str> = items
        .iter()
        .map(String::as_str)
        .filter(|item| !known.contains(item))
        .collect();
    orphans.sort_unstable();
    orphans.dedup();
    orphans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<String> {
        vec![
            "access.grants.read".to_string(),
            "access.grants.write".to_string(),
            "account.users.read".to_string(),
        ]
    }

    #[test]
    fn system_admin_group_resolves_to_the_whole_catalog() {
        let resolved = resolve_group_permissions(SYSTEM_ADMIN_GROUP_KEY, &[], &catalog());
        assert_eq!(resolved, catalog());
    }

    #[test]
    fn system_admin_ignores_its_own_stored_items() {
        // 内置组的条目由目录计算，即使表里意外有行也不参与。
        let resolved = resolve_group_permissions(
            SYSTEM_ADMIN_GROUP_KEY,
            &["demo.notes.read".to_string()],
            &catalog(),
        );
        assert_eq!(resolved, catalog());
        assert!(!resolved.iter().any(|p| p == "demo.notes.read"));
    }

    #[test]
    fn static_group_resolves_to_its_items_sorted_and_deduplicated() {
        let resolved = resolve_group_permissions(
            "ops",
            &[
                "access.grants.write".to_string(),
                "access.grants.read".to_string(),
                "access.grants.read".to_string(),
            ],
            &catalog(),
        );
        assert_eq!(
            resolved,
            ["access.grants.read", "access.grants.write"],
            "必须稳定排序并去重"
        );
    }

    #[test]
    fn orphan_items_are_reported_but_never_resolved_into_permissions() {
        // Review Focus 5：目录收缩后组里的权限条目成为孤儿。
        let items = vec![
            "demo.notes.read".to_string(),
            "access.grants.read".to_string(),
        ];
        let orphans = orphan_items(&items, &catalog());
        assert_eq!(orphans, ["demo.notes.read"], "孤儿条目必须被标记出来");

        // 关键：孤儿条目**不参与解析**，因此不会放大权限，也不会 panic。
        let resolved = resolve_group_permissions("ops", &items, &catalog());
        assert_eq!(resolved, ["access.grants.read"]);
    }

    #[test]
    fn static_group_with_only_orphans_resolves_to_nothing() {
        let resolved =
            resolve_group_permissions("ops", &["removed.module.act".to_string()], &catalog());
        assert!(resolved.is_empty());
    }

    #[test]
    fn catalog_permissions_is_fail_closed_when_not_installed() {
        // Review Focus 1：目录未安装时必须报错，绝不能退化成空集或全权集。
        use crate::addon::access::domain::permission_catalog::PermissionCatalogHandle;
        let handle = PermissionCatalogHandle::new();
        let error = match catalog_permissions(&handle) {
            Ok(permissions) => panic!("未安装目录必须失败，实际返回 {permissions:?}"),
            Err(error) => error,
        };
        assert!(
            matches!(error, yang_base::BaseError::ConfigError(_)),
            "必须是 ConfigError（fail-closed），实际为 {error:?}"
        );
    }
}
