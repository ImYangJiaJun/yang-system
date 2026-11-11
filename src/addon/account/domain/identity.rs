//! 外部身份绑定表 `user_identity`：系统用户 ↔ 外部 IdP 主体。
//!
//! 这是账号域的运行支撑表（与 `user_session` / `login_event` 同层），不进入
//! UI Catalog。`provider` + `tenant_key` + `external_id` 唯一决定一个外部身份；
//! `user_id` + `provider` + `tenant_key` 唯一决定一个用户在同一租户下的一个绑定。
//!
//! 所有读写以 `system` 能力执行：`external_email` 与 `external_id` 是身份事实，
//! 不能经普通角色字段权限暴露。

use std::sync::Arc;

use crate::addon::account::user::table::SYSTEM_ROLE;
use crate::db::Transaction;
use yang_base::action::ActionContext;
use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec, Timestamp};
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

/// 飞书提供方标识。
pub(crate) const PROVIDER_FEISHU: &str = "feishu";
/// 绑定状态：启用。
pub(crate) const IDENTITY_ACTIVE: &str = "active";

pub(crate) const IDENTITY_ID: &str = "id";
pub(crate) const IDENTITY_USER_ID: &str = "user_id";
pub(crate) const IDENTITY_PROVIDER: &str = "provider";
pub(crate) const IDENTITY_TENANT_KEY: &str = "tenant_key";
pub(crate) const IDENTITY_EXTERNAL_ID: &str = "external_id";
pub(crate) const IDENTITY_EXTERNAL_UNION_ID: &str = "external_union_id";
pub(crate) const IDENTITY_EXTERNAL_NAME: &str = "external_name";
pub(crate) const IDENTITY_EXTERNAL_EMAIL: &str = "external_email";
pub(crate) const IDENTITY_AVATAR_URL: &str = "avatar_url";
pub(crate) const IDENTITY_STATUS: &str = "status";
pub(crate) const IDENTITY_BOUND_AT: &str = "bound_at";
pub(crate) const IDENTITY_LAST_LOGIN_AT: &str = "last_login_at";

const IDENTITY_FIELDS: &[&str] = &[
    IDENTITY_ID,
    IDENTITY_USER_ID,
    IDENTITY_PROVIDER,
    IDENTITY_TENANT_KEY,
    IDENTITY_EXTERNAL_ID,
    IDENTITY_EXTERNAL_UNION_ID,
    IDENTITY_EXTERNAL_NAME,
    IDENTITY_EXTERNAL_EMAIL,
    IDENTITY_AVATAR_URL,
    IDENTITY_STATUS,
    IDENTITY_BOUND_AT,
    IDENTITY_LAST_LOGIN_AT,
];

/// 一次外部登录解析出的身份资料。
#[derive(Debug, Clone)]
pub(crate) struct ExternalIdentityProfile {
    pub(crate) provider: String,
    pub(crate) tenant_key: String,
    pub(crate) external_id: String,
    pub(crate) union_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) avatar_url: Option<String>,
}

impl ExternalIdentityProfile {
    pub(crate) fn feishu(
        tenant_key: impl Into<String>,
        open_id: impl Into<String>,
        union_id: Option<String>,
        name: Option<String>,
        email: Option<String>,
        avatar_url: Option<String>,
    ) -> Self {
        Self {
            provider: PROVIDER_FEISHU.to_string(),
            tenant_key: tenant_key.into(),
            external_id: open_id.into(),
            union_id,
            name,
            email,
            avatar_url,
        }
    }
}

/// 绑定行投影。
#[derive(Debug, Clone)]
pub(crate) struct IdentityRecord {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) provider: String,
    pub(crate) tenant_key: String,
    pub(crate) external_id: String,
    pub(crate) external_name: Option<String>,
    pub(crate) external_email: Option<String>,
    pub(crate) status: String,
    pub(crate) bound_at: i64,
    pub(crate) last_login_at: Option<i64>,
}

impl TryFrom<&Record> for IdentityRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(IDENTITY_ID)?,
            user_id: record.require(IDENTITY_USER_ID)?,
            provider: record.require(IDENTITY_PROVIDER)?,
            tenant_key: record.require(IDENTITY_TENANT_KEY)?,
            external_id: record.require(IDENTITY_EXTERNAL_ID)?,
            external_name: record.optional(IDENTITY_EXTERNAL_NAME)?,
            external_email: record.optional(IDENTITY_EXTERNAL_EMAIL)?,
            status: record.require(IDENTITY_STATUS)?,
            bound_at: record.require(IDENTITY_BOUND_AT)?,
            last_login_at: record.optional(IDENTITY_LAST_LOGIN_AT)?,
        })
    }
}

/// `user_identity` 的唯一 Schema 声明。
pub(crate) fn user_identity_table_spec() -> Result<TableSpec, BaseError> {
    let fields = yang_base::fields! {
        id => Key::new().title("ID").filterable(true),
        user_id => Int::new().title("系统用户 ID").require(true).filterable(true),
        provider => Str::new().title("身份提供方").require(true).max_length(32).filterable(true),
        tenant_key => Str::new()
            .title("外部租户")
            .require(true)
            .max_length(64)
            .default("")
            .filterable(true),
        external_id => Str::new()
            .title("外部主体 ID")
            .require(true)
            .max_length(128)
            .filterable(true),
        external_union_id => Str::new().title("外部 Union ID").max_length(128),
        external_name => Str::new().title("外部姓名").max_length(128),
        external_email => Str::new().title("外部邮箱").max_length(254).filterable(true),
        avatar_url => Str::new().title("外部头像 URL").max_length(1024),
        status => Str::new()
            .title("绑定状态")
            .require(true)
            .max_length(16)
            .default(IDENTITY_ACTIVE)
            .filterable(true),
        bound_at => Timestamp::new().title("绑定时间").require(true),
        last_login_at => Timestamp::new().title("最近登录时间"),
        created_at => Timestamp::new().title("创建时间").created_at(),
        updated_at => Timestamp::new().title("更新时间").updated_at(),
    };
    let table_name = TableName::new("user_identity")
        .map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(TableSpec::new(table_name.clone())
        .title("外部身份绑定")
        .fields(fields)
        .unique_named(
            "uk_user_identity_external",
            [
                field_ref(&table_name, IDENTITY_PROVIDER)?,
                field_ref(&table_name, IDENTITY_TENANT_KEY)?,
                field_ref(&table_name, IDENTITY_EXTERNAL_ID)?,
            ],
        )
        .unique_named(
            "uk_user_identity_user_provider",
            [
                field_ref(&table_name, IDENTITY_USER_ID)?,
                field_ref(&table_name, IDENTITY_PROVIDER)?,
                field_ref(&table_name, IDENTITY_TENANT_KEY)?,
            ],
        )
        .index_named(
            "idx_user_identity_email",
            [field_ref(&table_name, IDENTITY_EXTERNAL_EMAIL)?],
        )
        .check_named(
            "chk_user_identity_status",
            "`status` IN ('active', 'revoked')",
        ))
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

/// `user_identity` 的唯一持久化边界。
pub(crate) struct IdentityRepository {
    identities: TableDefinition,
}

impl IdentityRepository {
    pub(crate) fn new(identities: TableDefinition) -> Self {
        Self { identities }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.identities.bind(pool).query([SYSTEM_ROLE]))
    }

    pub(crate) async fn find_by_external(
        &self,
        ctx: &ActionContext,
        provider: &str,
        tenant_key: &str,
        external_id: &str,
    ) -> Result<Option<IdentityRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(IDENTITY_FIELDS)?
            .where_eq(
                IDENTITY_PROVIDER,
                serde_json::Value::String(provider.to_string()),
            )?
            .where_eq(
                IDENTITY_TENANT_KEY,
                serde_json::Value::String(tenant_key.to_string()),
            )?
            .where_eq(
                IDENTITY_EXTERNAL_ID,
                serde_json::Value::String(external_id.to_string()),
            )?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(IdentityRecord::try_from).transpose()
    }

    pub(crate) async fn find_by_user(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Vec<IdentityRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(IDENTITY_FIELDS)?
            .where_eq(IDENTITY_USER_ID, serde_json::Value::Number(user_id.into()))?
            .page(1, 100)?
            .all()
            .await?;
        rows.iter().map(IdentityRecord::try_from).collect()
    }

    pub(crate) async fn find_by_user_and_provider(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        provider: &str,
        tenant_key: &str,
    ) -> Result<Option<IdentityRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(IDENTITY_FIELDS)?
            .where_eq(IDENTITY_USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(
                IDENTITY_PROVIDER,
                serde_json::Value::String(provider.to_string()),
            )?
            .where_eq(
                IDENTITY_TENANT_KEY,
                serde_json::Value::String(tenant_key.to_string()),
            )?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(IdentityRecord::try_from).transpose()
    }

    pub(crate) async fn insert_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        profile: &ExternalIdentityProfile,
        now: i64,
    ) -> Result<i64, BaseError> {
        let record = Record::new()
            .set(IDENTITY_USER_ID, user_id)
            .set(IDENTITY_PROVIDER, profile.provider.as_str())
            .set(IDENTITY_TENANT_KEY, profile.tenant_key.as_str())
            .set(IDENTITY_EXTERNAL_ID, profile.external_id.as_str())
            .set(
                IDENTITY_EXTERNAL_UNION_ID,
                profile
                    .union_id
                    .as_deref()
                    .map(|value| serde_json::Value::String(value.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            )
            .set(
                IDENTITY_EXTERNAL_NAME,
                profile
                    .name
                    .as_deref()
                    .map(|value| serde_json::Value::String(value.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            )
            .set(
                IDENTITY_EXTERNAL_EMAIL,
                profile
                    .email
                    .as_deref()
                    .map(|value| serde_json::Value::String(value.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            )
            .set(
                IDENTITY_AVATAR_URL,
                profile
                    .avatar_url
                    .as_deref()
                    .map(|value| serde_json::Value::String(value.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            )
            .set(IDENTITY_STATUS, IDENTITY_ACTIVE)
            .set(IDENTITY_BOUND_AT, now);
        let (_, id) = self
            .trusted_query(ctx)?
            .insert_returning_id_in_tx(transaction, record)
            .await?;
        i64::try_from(id).map_err(|_| BaseError::Unknown("身份主键超出 i64 范围".to_string()))
    }

    pub(crate) async fn touch_login_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        identity_id: i64,
        profile: &ExternalIdentityProfile,
        now: i64,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(IDENTITY_ID, serde_json::Value::Number(identity_id.into()))?
            .update_in_tx(
                transaction,
                Record::new()
                    .set(IDENTITY_LAST_LOGIN_AT, now)
                    .set(
                        IDENTITY_EXTERNAL_NAME,
                        profile
                            .name
                            .as_deref()
                            .map(|value| serde_json::Value::String(value.to_string()))
                            .unwrap_or(serde_json::Value::Null),
                    )
                    .set(
                        IDENTITY_EXTERNAL_EMAIL,
                        profile
                            .email
                            .as_deref()
                            .map(|value| serde_json::Value::String(value.to_string()))
                            .unwrap_or(serde_json::Value::Null),
                    )
                    .set(
                        IDENTITY_AVATAR_URL,
                        profile
                            .avatar_url
                            .as_deref()
                            .map(|value| serde_json::Value::String(value.to_string()))
                            .unwrap_or(serde_json::Value::Null),
                    ),
            )
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("身份 {identity_id} 登录信息更新未精确影响一行"),
            )));
        }
        Ok(())
    }

    pub(crate) async fn delete_by_user_provider_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        provider: &str,
        tenant_key: &str,
    ) -> Result<u64, BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(IDENTITY_USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(
                IDENTITY_PROVIDER,
                serde_json::Value::String(provider.to_string()),
            )?
            .where_eq(
                IDENTITY_TENANT_KEY,
                serde_json::Value::String(tenant_key.to_string()),
            )?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }

    /// 账号删除时清理该用户全部外部绑定；凭证由调用方先行清理。
    pub(crate) async fn delete_by_user_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<u64, BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(IDENTITY_USER_ID, serde_json::Value::Number(user_id.into()))?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> TableDefinition {
        user_identity_table_spec()
            .unwrap_or_else(|error| panic!("user_identity 表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("user_identity 表定义应有效: {error}"))
    }

    #[test]
    fn table_has_expected_name_primary_key_and_unique_indexes() {
        let definition = definition();
        assert_eq!(definition.name(), "user_identity");
        assert_eq!(definition.primary_key(), "id");
        let spec =
            user_identity_table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        assert!(spec.indexes.iter().any(
            |index| index.unique && index.name.as_deref() == Some("uk_user_identity_external")
        ));
        assert!(spec
            .indexes
            .iter()
            .any(|index| index.unique
                && index.name.as_deref() == Some("uk_user_identity_user_provider")));
        assert!(
            spec.indexes
                .iter()
                .any(|index| !index.unique
                    && index.name.as_deref() == Some("idx_user_identity_email"))
        );
    }

    #[test]
    fn tenant_key_is_required_with_empty_default() {
        let spec =
            user_identity_table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        let field = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == IDENTITY_TENANT_KEY)
            .unwrap_or_else(|| panic!("tenant_key 字段必须存在"));
        assert!(field.storage.required);
        assert_eq!(field.storage.default, Some(serde_json::json!("")));
    }
}
