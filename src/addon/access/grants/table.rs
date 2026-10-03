//! authz_grant 表声明：用户 ↔ 权限字符串直授关系（决策 D4，无角色聚合层）。
//!
//! 声明（模块是什么）位于模块层；机制（模块怎么做）位于 `domain/`。
//! 授权事实只能经 `domain/repository.rs` 的受信 writer 变更。

use super::super::domain::permission_catalog::{PERMISSION_MAX_LENGTH, PERMISSION_PATTERN};
use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::BaseError;

pub(crate) const SYSTEM_ROLE: &str = "system";
pub(crate) const GRANT_ID: &str = "id";
pub(crate) const USER_ID: &str = "user_id";
pub(crate) const PERMISSION: &str = "permission";
pub(crate) const GRANTED_BY: &str = "granted_by";
pub(crate) const OCCURRED_AT: &str = "occurred_at";
pub(crate) const EXPIRES_AT: &str = "expires_at";
pub(crate) const GRANT_RECORD_FIELDS: &[&str] = &[
    GRANT_ID,
    USER_ID,
    PERMISSION,
    GRANTED_BY,
    OCCURRED_AT,
    EXPIRES_AT,
];

/// 构建授权事实表的唯一 Schema 定义。
pub(crate) fn grants_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true),
        user_id => Int::new()
                .title("用户 ID")
                .require(true)
                .filterable(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        permission => Str::new()
                .title("权限")
                .require(true)
                .max_length(PERMISSION_MAX_LENGTH)
                .pattern(PERMISSION_PATTERN)
                .filterable(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        granted_by => Int::new()
                .title("授权操作人")
                .require(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        occurred_at => Timestamp::new()
                .title("授权时间")
                .created_at()
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        expires_at => Int::new()
                .title("过期时间")
                .description("Unix 秒；NULL=永久有效。过期后权限在解析侧失效、行保留做审计")
                .filterable(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
    };
    let table_name =
        TableName::new("authz_grant").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("授权记录")
        .fields(fields)
        .unique_named(
            "uk_authz_grant_user_permission",
            [
                field_ref(&table_name, USER_ID)?,
                field_ref(&table_name, PERMISSION)?,
            ],
        )
        // 反查索引：按 permission 检索命中该权限的全部授予行（权限下钻/审计反查）。
        // 复合唯一键前缀是 user_id，覆盖不了 `WHERE permission = ?` 这类查询。
        .index_named(
            "idx_authz_grant_permission",
            [field_ref(&table_name, PERMISSION)?],
        )
        // 审计检索索引：按授权操作人追溯授权历史（`WHERE granted_by = ?`），避免全表扫。
        .index_named(
            "idx_authz_grant_granted_by",
            [field_ref(&table_name, GRANTED_BY)?],
        )
        .check_named(
            "chk_authz_grant_permission_format",
            "regexp_like(`permission`, '^[a-z][a-z0-9_]*(\\\\.[a-z][a-z0-9_]*)+$')",
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use yang_base::table::FieldType;

    #[test]
    fn grant_schema_has_composite_unique_and_system_only_fields() {
        let spec = grants_table_spec().unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let permission_spec = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == PERMISSION)
            .unwrap_or_else(|| panic!("应存在 permission 字段"));
        assert_eq!(
            permission_spec.validation.pattern.as_deref(),
            Some(PERMISSION_PATTERN)
        );
        assert!(spec.indexes.iter().any(|index| index.unique
            && index.name.as_deref() == Some("uk_authz_grant_user_permission")
            && index.fields.len() == 2));
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let id = definition
            .field(GRANT_ID)
            .unwrap_or_else(|| panic!("应存在 id 字段"));

        assert_eq!(definition.name(), "authz_grant");
        assert_eq!(definition.primary_key(), GRANT_ID);
        assert!(id.is_auto_increment());
    }

    #[test]
    fn grant_schema_declares_nullable_expiry_for_audit_retention() {
        let spec = grants_table_spec().unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let expires = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == EXPIRES_AT)
            .unwrap_or_else(|| panic!("应存在 expires_at 字段"));
        assert!(
            !expires.storage.required,
            "expires_at 必须可空（NULL=永久）"
        );
        assert!(expires.access.filterable, "过期过滤需要 SQL 层筛选能力");

        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let expires = definition
            .field(EXPIRES_AT)
            .unwrap_or_else(|| panic!("应存在 expires_at 字段"));
        assert_eq!(
            expires.field_type(),
            &FieldType::BigInt,
            "expires_at 存储为 64 位整数（unix 秒）"
        );
        assert!(!expires.is_required(), "expires_at 必须可空（NULL=永久）");
    }

    #[tokio::test]
    async fn grant_facts_are_only_readable_and_writable_by_system_role() {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition = grants_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let table = definition.bind(Arc::new(pool));

        for field_name in [USER_ID, PERMISSION, GRANTED_BY, OCCURRED_AT, EXPIRES_AT] {
            let denied = table.query(["user"]).select_fields(&[field_name]);
            assert!(matches!(
                denied,
                Err(BaseError::FieldPermissionDenied(_, field, _)) if field == field_name
            ));
            assert!(table
                .query([SYSTEM_ROLE])
                .select_fields(&[field_name])
                .is_ok());
        }

        let denied_write = table
            .query(["user"])
            .insert(yang_base::table::Record::new().set(USER_ID, 7_i64))
            .await;
        assert!(matches!(
            denied_write,
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == USER_ID
        ));
    }
}
