//! 用户表 Schema 与对外 DTO。

use super::policy::{USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH, USERNAME_PATTERN};
use super::status::UserStatus;
use schemars::JsonSchema;
use serde::Serialize;
use yang_base::definition::{Int, Key, Radio, Str, TableName, TableSpec, Timestamp};
use yang_base::table::Record;
use yang_base::BaseError;

pub(super) const SYSTEM_ROLE: &str = "system";
pub(super) const USER_ID: &str = "id";
pub(super) const USERNAME: &str = "username";
pub(super) const EMAIL: &str = "email";
pub(super) const EMAIL_VERIFIED_AT: &str = "email_verified_at";
pub(super) const PASSWORD_HASH: &str = "password_hash";
pub(super) const STATUS: &str = "status";
pub(super) const AUTHZ_VERSION: &str = "authz_version";
pub(super) const CREDENTIAL_VERSION: &str = "credential_version";
pub(super) const CREATED_AT: &str = "created_at";
pub(super) const UPDATED_AT: &str = "updated_at";
pub(super) const USER_VIEW_FIELDS: &[&str] = &[
    USER_ID,
    USERNAME,
    EMAIL,
    EMAIL_VERIFIED_AT,
    STATUS,
    CREATED_AT,
    UPDATED_AT,
];

/// 可安全返回给客户端的用户视图，不包含密码摘要。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct UserView {
    id: i64,
    username: String,
    email: Option<String>,
    email_verified_at: Option<i64>,
    status: UserStatus,
    created_at: i64,
    updated_at: i64,
}

impl TryFrom<&Record> for UserView {
    type Error = BaseError;

    fn try_from(user: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: user.require(USER_ID)?,
            username: user.require(USERNAME)?,
            email: user.optional(EMAIL)?,
            email_verified_at: user.optional(EMAIL_VERIFIED_AT)?,
            status: UserStatus::from_storage(&user.require::<String>(STATUS)?)?,
            created_at: user.require(CREATED_AT)?,
            updated_at: user.require(UPDATED_AT)?,
        })
    }
}

/// 构建用户表的唯一 Schema 定义。
pub(super) fn user_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true),
        username => Str::new()
                .title("用户名")
                .require(true)
                .min_length(USERNAME_MIN_LENGTH)
                .max_length(USERNAME_MAX_LENGTH)
                .pattern(USERNAME_PATTERN)
                .unique(true)
                .filterable(true),
        email => Str::new()
                .title("已验证邮箱")
                .max_length(254)
                .email()
                .unique(true)
                .filterable(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        email_verified_at => Timestamp::new()
                .title("邮箱验证时间")
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        password_hash => Str::new()
                .title("密码摘要")
                .require(true)
                .max_length(255)
                .secret(true)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        status => Radio::<UserStatus>::new()
                .title("状态")
                .require(true)
                .varchar(16)
                .options([
                    (UserStatus::Active, "启用"),
                    (UserStatus::Disabled, "停用"),
                ]),
        authz_version => Int::new()
                .title("授权版本")
                .require(true)
                .default(1_i64)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
        credential_version => Int::new()
                .title("凭据版本")
                .require(true)
                .default(0_i64)
                .readable_by([SYSTEM_ROLE])
                .writable_by([SYSTEM_ROLE]),
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
    fn user_schema_uses_generated_id_and_protects_internal_fields() {
        let spec = user_table_spec().unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let status_spec = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == STATUS)
            .unwrap_or_else(|| panic!("应存在 status 字段"));
        assert_eq!(status_spec.kind, yang_base::definition::FieldKind::Radio);
        assert_eq!(
            status_spec.options,
            [
                ("active".to_string(), "启用".to_string()),
                ("disabled".to_string(), "停用".to_string()),
            ]
        );
        let definition = spec
            .table_definition()
            .unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let id = definition
            .field(USER_ID)
            .unwrap_or_else(|| panic!("应存在 id 字段"));
        let password = definition
            .field(PASSWORD_HASH)
            .unwrap_or_else(|| panic!("应存在 password_hash 字段"));
        let username = definition
            .field(USERNAME)
            .unwrap_or_else(|| panic!("应存在 username 字段"));
        let email = definition
            .field(EMAIL)
            .unwrap_or_else(|| panic!("应存在 email 字段"));
        let email_verified_at = definition
            .field(EMAIL_VERIFIED_AT)
            .unwrap_or_else(|| panic!("应存在 email_verified_at 字段"));
        let authz_version = definition
            .field(AUTHZ_VERSION)
            .unwrap_or_else(|| panic!("应存在 authz_version 字段"));
        let credential_version = definition
            .field(CREDENTIAL_VERSION)
            .unwrap_or_else(|| panic!("应存在 credential_version 字段"));

        assert_eq!(definition.name(), "users");
        assert_eq!(definition.primary_key(), USER_ID);
        assert!(id.is_auto_increment());
        assert!(id.is_filterable());
        assert!(username.is_filterable());
        assert!(email.is_filterable());
        assert!(!email_verified_at.is_filterable());
        assert!(!password.is_filterable());
        assert!(!password.is_sortable());
        assert_eq!(
            authz_version.default_value(),
            Some(&serde_json::json!(1_i64))
        );
        assert!(!authz_version.is_filterable());
        assert!(!authz_version.is_sortable());
        assert_eq!(
            credential_version.default_value(),
            Some(&serde_json::json!(0_i64))
        );
        assert!(!credential_version.is_filterable());
        assert!(!credential_version.is_sortable());
        assert!(!USER_VIEW_FIELDS.contains(&AUTHZ_VERSION));
        assert!(!USER_VIEW_FIELDS.contains(&CREDENTIAL_VERSION));
    }

    #[tokio::test]
    async fn internal_fields_are_only_readable_and_writable_by_system_role() {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition = user_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let table = definition.bind(Arc::new(pool));

        for field_name in [
            EMAIL,
            EMAIL_VERIFIED_AT,
            PASSWORD_HASH,
            AUTHZ_VERSION,
            CREDENTIAL_VERSION,
        ] {
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
            .insert(Record::new().set(AUTHZ_VERSION, 2_i64))
            .await;
        assert!(matches!(
            denied_write,
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == AUTHZ_VERSION
        ));
    }
}
