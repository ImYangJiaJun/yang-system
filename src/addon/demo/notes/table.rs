//! demo_note 表声明：便签业务的唯一 Schema 事实来源。
//!
//! 声明（模块是什么）位于模块层；机制（模块怎么做）位于 `domain/`。
//! 所有权边界：`owner_user_id` 只允许 system 角色写入，业务事实只能经
//! `domain/repository.rs` 的受信 writer 变更，客户端永远不能直接指定归属人。

use yang_base::definition::{
    FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Text, Timestamp,
};
use yang_base::BaseError;

pub(crate) const SYSTEM_ROLE: &str = "system";
pub(crate) const TABLE_NAME: &str = "demo_note";
pub(crate) const NOTE_ID: &str = "id";
pub(crate) const OWNER_USER_ID: &str = "owner_user_id";
pub(crate) const TITLE: &str = "title";
pub(crate) const CONTENT: &str = "content";
pub(crate) const CREATED_AT: &str = "created_at";
pub(crate) const UPDATED_AT: &str = "updated_at";
pub(crate) const TITLE_MAX_LENGTH: usize = 128;
pub(crate) const CONTENT_MAX_LENGTH: usize = 4096;

/// 构建便签表的唯一 Schema 定义。
pub(crate) fn notes_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true),
        owner_user_id => Int::new()
                .title("归属用户")
                .require(true)
                .filterable(true)
                .writable_by([SYSTEM_ROLE]),
        title => Str::new()
                .title("标题")
                .require(true)
                .min_length(1)
                .max_length(TITLE_MAX_LENGTH)
                .searchable(true)
                .filterable(true)
                .sortable(true),
        content => Text::new()
                .title("内容")
                .sortable(false),
        created_at => Timestamp::new()
                .title("创建时间")
                .created_at()
                .sortable(true),
        updated_at => Timestamp::new()
                .title("更新时间")
                .updated_at(),
    };
    let table_name =
        TableName::new(TABLE_NAME).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("便签")
        .fields(fields)
        .index_named(
            "idx_demo_note_owner",
            [field_ref(&table_name, OWNER_USER_ID)?],
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

    #[test]
    fn note_schema_has_owner_index_and_generated_timestamps() {
        let spec = notes_table_spec().unwrap_or_else(|error| panic!("便签表定义应有效: {error}"));
        let title_spec = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == TITLE)
            .unwrap_or_else(|| panic!("应存在 title 字段"));
        assert!(title_spec.access.searchable);
        assert!(title_spec.access.filterable);
        assert!(title_spec.access.sortable);
        assert_eq!(title_spec.validation.max_length, Some(TITLE_MAX_LENGTH));
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("便签表定义应有效: {error}"));
        let id = definition
            .field(NOTE_ID)
            .unwrap_or_else(|| panic!("应存在 id 字段"));
        let created_at = definition
            .field(CREATED_AT)
            .unwrap_or_else(|| panic!("应存在 created_at 字段"));

        assert_eq!(definition.name(), TABLE_NAME);
        assert_eq!(definition.primary_key(), NOTE_ID);
        assert!(id.is_auto_increment());
        assert!(created_at.is_sortable());
        assert!(spec.indexes.iter().any(|index| !index.unique
            && index.name.as_deref() == Some("idx_demo_note_owner")
            && index.fields.len() == 1));
    }

    #[tokio::test]
    async fn owner_field_is_only_writable_by_system_role() {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition = notes_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("便签表定义应有效: {error}"));
        let table = definition.bind(Arc::new(pool));

        // 普通用户角色永远不能写入归属人字段，防止客户端伪造所有权。
        let denied_write = table
            .query(["user"])
            .insert(yang_base::table::Record::new().set(OWNER_USER_ID, 7_i64))
            .await;
        assert!(matches!(
            denied_write,
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == OWNER_USER_ID
        ));
        // 受信 writer（system 角色）可以写入归属人。
        assert!(table
            .query([SYSTEM_ROLE])
            .insert(yang_base::table::Record::new())
            .await
            .is_err_and(|error| !matches!(error, BaseError::FieldPermissionDenied(_, _, _))));
        // 普通用户角色可以按归属人过滤（列表所有权 scoped 查询依赖此能力）。
        let scoped = table
            .query(["user"])
            .select_fields(&[NOTE_ID, TITLE])
            .and_then(|query| {
                query.where_eq(OWNER_USER_ID, serde_json::Value::Number(7_i64.into()))
            });
        assert!(scoped.is_ok());
    }
}
