//! 用户表 Schema 与对外 DTO。

use schemars::JsonSchema;
use serde::Serialize;
use yang_base::definition::{Key, Str, TableName, TableSpec, Timestamp};
use yang_base::table::Record;
use yang_base::BaseError;

pub(super) const SYSTEM_ROLE: &str = "system";
pub(super) const USER_ID: &str = "id";
pub(super) const USERNAME: &str = "username";
pub(super) const PASSWORD_HASH: &str = "password_hash";
pub(super) const STATUS: &str = "status";
pub(super) const CREATED_AT: &str = "created_at";
pub(super) const UPDATED_AT: &str = "updated_at";
pub(super) const USER_VIEW_FIELDS: &[&str] = &[USER_ID, USERNAME, STATUS, CREATED_AT, UPDATED_AT];

/// 可安全返回给客户端的用户视图，不包含密码摘要。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct UserView {
    id: i64,
    username: String,
    status: String,
    created_at: i64,
    updated_at: i64,
}

impl TryFrom<&Record> for UserView {
    type Error = BaseError;

    fn try_from(user: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: user.require(USER_ID)?,
            username: user.require(USERNAME)?,
            status: user.require(STATUS)?,
            created_at: user.require(CREATED_AT)?,
            updated_at: user.require(UPDATED_AT)?,
        })
    }
}

/// 构建用户表的唯一 Schema 定义。
pub(super) fn user_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID"),
        username => Str::new()
                .title("用户名")
                .require(true)
                .max_length(64)
                .unique(true),
        password_hash => Str::new()
                .title("密码摘要")
                .require(true)
                .max_length(255)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        status => Str::new().title("状态").require(true).max_length(16),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at(),
    };
    let table_name =
        TableName::new("users").map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name).title("用户").fields(fields))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn user_schema_uses_generated_id_and_protects_password_hash() {
        let definition = user_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let id = definition
            .field(USER_ID)
            .unwrap_or_else(|| panic!("应存在 id 字段"));
        let password = definition
            .field(PASSWORD_HASH)
            .unwrap_or_else(|| panic!("应存在 password_hash 字段"));

        assert_eq!(definition.name(), "users");
        assert_eq!(definition.primary_key(), USER_ID);
        assert!(id.is_auto_increment());
        assert!(!password.is_filterable());
        assert!(!password.is_sortable());
    }

    #[tokio::test]
    async fn password_hash_is_only_readable_by_system_role() {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition = user_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let table = definition.bind(Arc::new(pool));

        let denied = table.query(["user"]).select_fields(&[PASSWORD_HASH]);
        assert!(matches!(
            denied,
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == PASSWORD_HASH
        ));
        assert!(table
            .query([SYSTEM_ROLE])
            .select_fields(&[PASSWORD_HASH])
            .is_ok());
    }
}
