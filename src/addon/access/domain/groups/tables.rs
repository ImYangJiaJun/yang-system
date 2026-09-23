//! 权限组的三张运行支撑表声明：组-权限条目、用户-组关系、引导哨兵。
//!
//! 这三张表没有独立 UI 语义，因此按 `docs/contracts/SCHEMA.md` 与
//! `src/infrastructure/schema.rs` 的既有惯例放进运行支撑表数组，
//! 而不是各建一个 module（一 Module 一表）。

// 组事实的列名常量随表声明一并落地；大部分消费者（受信 writer 在 Task 3+、
// 引导哨兵写入在 Task 8+）晚于本任务接入，故显式豁免 dead-code 门禁。
#![allow(dead_code)]

use crate::addon::access::domain::permission_catalog::{PERMISSION_MAX_LENGTH, PERMISSION_PATTERN};
use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

/// 系统角色名：与 `access/grants/table.rs`、`access/groups/table.rs` 同名同值，
/// 各表声明文件按仓库既有惯例各自私有定义。
pub(crate) const SYSTEM_ROLE: &str = "system";

pub(crate) const ITEM_ID: &str = "id";
pub(crate) const ITEM_GROUP_ID: &str = "group_id";
pub(crate) const ITEM_PERMISSION: &str = "permission";
pub(crate) const ITEM_GRANTED_BY: &str = "granted_by";
pub(crate) const ITEM_OCCURRED_AT: &str = "occurred_at";

pub(crate) const MEMBER_ID: &str = "id";
pub(crate) const MEMBER_USER_ID: &str = "user_id";
pub(crate) const MEMBER_GROUP_ID: &str = "group_id";
pub(crate) const MEMBER_GRANTED_BY: &str = "granted_by";
pub(crate) const MEMBER_OCCURRED_AT: &str = "occurred_at";

pub(crate) const OWNER_ID: &str = "id";
pub(crate) const OWNER_SENTINEL_KEY: &str = "sentinel_key";
pub(crate) const OWNER_USER_ID: &str = "user_id";
pub(crate) const OWNER_CLAIMED_AT: &str = "claimed_at";

/// 哨兵行唯一取值。第二个插入者必然违反 UNIQUE 或 CHECK。
pub(crate) const SENTINEL_KEY_VALUE: &str = "system-owner";
const SENTINEL_KEY_MAX_LENGTH: usize = 32;
const SENTINEL_CHECK_EXPR: &str = "`sentinel_key` = 'system-owner'";

fn table_name(raw: &str) -> Result<TableName, BaseError> {
    TableName::new(raw).map_err(|error| BaseError::ConfigError(error.to_string()))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

/// 组 → 权限条目。
pub(crate) fn group_items_table_spec() -> Result<TableSpec, BaseError> {
    let name = table_name("permission_group_item")?;
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        group_id => Int::new().title("权限组").require(true).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        permission => Str::new().title("权限").require(true)
                .max_length(PERMISSION_MAX_LENGTH).pattern(PERMISSION_PATTERN).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        granted_by => Int::new().title("授权操作人").require(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new().title("授权时间").created_at()
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
    };
    Ok(TableSpec::new(name.clone())
        .title("权限组条目")
        .fields(fields)
        .unique_named(
            "uk_permission_group_item",
            [
                field_ref(&name, ITEM_GROUP_ID)?,
                field_ref(&name, ITEM_PERMISSION)?,
            ],
        )
        .check_named(
            "chk_permission_group_item_permission_format",
            "regexp_like(`permission`, '^[a-z][a-z0-9_]*(\\\\.[a-z][a-z0-9_]*)+$')",
        ))
}

/// 用户 → 权限组。
pub(crate) fn user_group_table_spec() -> Result<TableSpec, BaseError> {
    let name = table_name("user_group")?;
    let users = table_name("users")?;
    let groups = table_name("permission_group")?;
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        user_id => Int::new().title("用户").require(true).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        group_id => Int::new().title("权限组").require(true).filterable(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        granted_by => Int::new().title("授权操作人").require(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new().title("入组时间").created_at()
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
    };
    Ok(TableSpec::new(name.clone())
        .title("用户权限组")
        .fields(fields)
        .unique_named(
            "uk_user_group",
            [
                field_ref(&name, MEMBER_USER_ID)?,
                field_ref(&name, MEMBER_GROUP_ID)?,
            ],
        )
        // 外键规则固定 RESTRICT：删除仍有成员的组会被数据库拒绝（spec §8.3）。
        .foreign_key_named(
            "fk_user_group_user",
            [field_ref(&name, MEMBER_USER_ID)?],
            [field_ref(&users, "id")?],
        )
        .foreign_key_named(
            "fk_user_group_group",
            [field_ref(&name, MEMBER_GROUP_ID)?],
            [field_ref(&groups, "id")?],
        ))
}

/// 引导哨兵：语义上的单行表。
pub(crate) fn system_owner_table_spec() -> Result<TableSpec, BaseError> {
    let name = table_name("system_owner")?;
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        sentinel_key => Str::new().title("哨兵键").require(true)
                .max_length(SENTINEL_KEY_MAX_LENGTH)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        user_id => Int::new().title("被引导用户").require(true)
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
        claimed_at => Timestamp::new().title("声明时间").created_at()
                .readable_by([SYSTEM_ROLE]).writable_by([SYSTEM_ROLE]),
    };
    Ok(TableSpec::new(name.clone())
        .title("系统管理员声明")
        .fields(fields)
        .unique_named(
            "uk_system_owner_sentinel",
            [field_ref(&name, OWNER_SENTINEL_KEY)?],
        )
        .check_named("chk_system_owner_sentinel", SENTINEL_CHECK_EXPR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_item_table_has_composite_unique_and_permission_check() {
        let spec = group_items_table_spec().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.name.as_str(), "permission_group_item");
        assert!(spec
            .indexes
            .iter()
            .any(|i| i.unique && i.name.as_deref() == Some("uk_permission_group_item")));
        assert!(spec
            .checks
            .iter()
            .any(|c| c.name == "chk_permission_group_item_permission_format"));
    }

    #[test]
    fn user_group_table_has_two_restrict_foreign_keys() {
        let spec = user_group_table_spec().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.name.as_str(), "user_group");
        let fks: Vec<&str> = spec.foreign_keys.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fks, ["fk_user_group_user", "fk_user_group_group"]);
        // 外键必须指向 users.id 与 permission_group.id，而不是任意同名列。
        let targets: Vec<String> = spec
            .foreign_keys
            .iter()
            .map(|f| {
                let columns: Vec<String> = f
                    .referenced_fields
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                format!("{}->{}", f.name, columns.join(","))
            })
            .collect();
        assert_eq!(
            targets,
            [
                "fk_user_group_user->users.id",
                "fk_user_group_group->permission_group.id"
            ]
        );
    }

    #[test]
    fn system_owner_table_pins_the_sentinel_key() {
        let spec = system_owner_table_spec().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.name.as_str(), "system_owner");
        assert!(spec
            .indexes
            .iter()
            .any(|i| i.unique && i.name.as_deref() == Some("uk_system_owner_sentinel")));
        // CHECK 必须真的锁死哨兵取值；只断言约束名等于没断言（引导并发安全依赖此表达式）。
        let check = spec
            .checks
            .iter()
            .find(|c| c.name == "chk_system_owner_sentinel")
            .unwrap_or_else(|| panic!("应存在 chk_system_owner_sentinel"));
        assert!(
            check.expression.contains(SENTINEL_KEY_VALUE)
                && check.expression.contains(OWNER_SENTINEL_KEY),
            "哨兵 CHECK 应锁死 {} = '{}'，实际为 {}",
            OWNER_SENTINEL_KEY,
            SENTINEL_KEY_VALUE,
            check.expression
        );
    }
}
