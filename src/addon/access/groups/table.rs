//! permission_group 表声明：权限组本体（决策 D4 扩展层）。
//!
//! 声明（模块是什么）位于模块层；机制（模块怎么做）位于 `domain/groups/`。
//! 授权事实只能经 `domain/groups/repository.rs` 的受信 writer 变更。

// 组事实的列名常量随表声明一并落地；消费者（受信 writer 在 Task 3+、
// 组 Action 在 Task 10+）晚于本任务接入，故显式豁免 dead-code 门禁。
#![allow(dead_code)]

use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

/// 组事实表字段只对系统角色读写：业务读写一律走受信 writer。
pub(crate) const SYSTEM_ROLE: &str = "system";

/// 组标识格式：单段小写，用于代码与审计引用（展示名走 `title`）。
pub(crate) const GROUP_KEY_PATTERN: &str = r"^[a-z][a-z0-9_]*$";
pub(crate) const GROUP_KEY_MAX_LENGTH: usize = 64;

pub(crate) const GROUP_ID: &str = "id";
pub(crate) const GROUP_KEY: &str = "group_key";
pub(crate) const GROUP_TITLE: &str = "title";
pub(crate) const GROUP_DESCRIPTION: &str = "description";
pub(crate) const GROUP_CREATED_BY: &str = "created_by";
pub(crate) const GROUP_OCCURRED_AT: &str = "occurred_at";
pub(crate) const GROUP_RECORD_FIELDS: &[&str] = &[
    GROUP_ID,
    GROUP_KEY,
    GROUP_TITLE,
    GROUP_DESCRIPTION,
    GROUP_CREATED_BY,
    GROUP_OCCURRED_AT,
];

/// 构建权限组事实表的唯一 Schema 定义。
pub(crate) fn groups_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        // 主键必须可筛选：受信 writer 的按 id 读取（`find_by_id_in_tx`）依赖它，
        // 与 account/user、access/grants 两张表同例。
        id => Key::new().title("ID").filterable(true),
        group_key => Str::new()
                .title("组标识")
                .require(true)
                .max_length(GROUP_KEY_MAX_LENGTH)
                .pattern(GROUP_KEY_PATTERN)
                .filterable(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        title => Str::new()
                .title("展示名")
                .require(true)
                .max_length(128)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        description => Str::new()
                .title("描述")
                .max_length(255)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        created_by => Int::new()
                .title("创建人")
                .require(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new()
                .title("创建时间")
                .created_at()
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
    };
    let table_name =
        TableName::new("permission_group").map_err(|e| BaseError::ConfigError(e.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("权限组")
        .fields(fields)
        .unique_named(
            "uk_permission_group_key",
            [field_ref(&table_name, GROUP_KEY)?],
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|e| BaseError::ConfigError(e.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_table_declares_expected_columns_and_unique_key() {
        let spec = groups_table_spec().unwrap_or_else(|e| panic!("组表定义应有效: {e}"));
        let names: Vec<&str> = spec.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "id",
                "group_key",
                "title",
                "description",
                "created_by",
                "occurred_at"
            ]
        );
        assert!(spec
            .indexes
            .iter()
            .any(|i| i.unique && i.name.as_deref() == Some("uk_permission_group_key")));

        let definition = spec.table_definition().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(definition.name(), "permission_group");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn group_key_pattern_rejects_uppercase_and_dots() {
        let spec = groups_table_spec().unwrap_or_else(|e| panic!("{e}"));
        let key = spec
            .fields
            .iter()
            .find(|f| f.name.as_str() == "group_key")
            .unwrap_or_else(|| panic!("应存在 group_key 字段"));
        assert_eq!(key.validation.pattern.as_deref(), Some(GROUP_KEY_PATTERN));
        assert_eq!(key.validation.max_length, Some(GROUP_KEY_MAX_LENGTH));

        // 模式必须真的拒绝大写与前导数字，而不是仅被写进字段声明。
        let pattern = yang_base::table::Validator::Regex(GROUP_KEY_PATTERN.to_string());
        assert!(pattern
            .validate(GROUP_KEY, &serde_json::json!("team_admins"))
            .is_ok());
        assert!(pattern
            .validate(GROUP_KEY, &serde_json::json!("Team.Admins"))
            .is_err());
        assert!(pattern
            .validate(GROUP_KEY, &serde_json::json!("2fa"))
            .is_err());
    }
}
