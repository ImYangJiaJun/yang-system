//! 用户聚合的唯一持久化边界。
//!
//! 对外 `TableQuery` 始终遵守请求用户的字段权限；只有本 Repository 能以
//! `system` 能力读写密码摘要，避免公开注册和登录被字段权限拦截，也避免把通用
//! 提权查询暴露给 Action。
//! authorization-writer: account-user-facts

use super::schema::{
    AUTHZ_VERSION, CREDENTIAL_VERSION, EMAIL, EMAIL_VERIFIED_AT, PASSWORD_HASH, STATUS,
    SYSTEM_ROLE, USERNAME, USER_ID, USER_VIEW_FIELDS,
};
use super::status::UserStatus;
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

const USER_CREDENTIAL_FIELDS: &[&str] = &[USER_ID, PASSWORD_HASH, STATUS];
const USER_AUTHORIZATION_FIELDS: &[&str] = &[USERNAME, STATUS, AUTHZ_VERSION, CREDENTIAL_VERSION];

pub(super) struct CredentialRecord {
    pub(super) id: i64,
    pub(super) password_hash: String,
    pub(super) status: UserStatus,
}

impl TryFrom<&Record> for CredentialRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(USER_ID)?,
            password_hash: record.require(PASSWORD_HASH)?,
            status: UserStatus::from_storage(&record.require::<String>(STATUS)?)?,
        })
    }
}

pub(super) struct AuthorizationStateRecord {
    pub(super) username: String,
    pub(super) status: UserStatus,
    pub(super) authz_version: i64,
    pub(super) credential_version: i64,
}

impl TryFrom<&Record> for AuthorizationStateRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            username: record.require(USERNAME)?,
            status: UserStatus::from_storage(&record.require::<String>(STATUS)?)?,
            authz_version: record.require(AUTHZ_VERSION)?,
            credential_version: record.require(CREDENTIAL_VERSION)?,
        })
    }
}

pub(super) struct UserRepository {
    users: TableDefinition,
}

impl UserRepository {
    pub(super) fn new(users: TableDefinition) -> Self {
        Self { users }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.users.bind(pool).query([SYSTEM_ROLE]))
    }

    pub(super) async fn username_exists(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(USERNAME, serde_json::Value::String(username.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    pub(super) async fn email_exists(
        &self,
        ctx: &ActionContext,
        email: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(EMAIL, serde_json::Value::String(email.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    pub(super) async fn find_credentials_by_username(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<Option<CredentialRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USERNAME, serde_json::Value::String(username.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(CredentialRecord::try_from).transpose()
    }

    pub(super) async fn find_credentials_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<Option<CredentialRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(CredentialRecord::try_from).transpose()
    }

    pub(super) async fn find_by_id(
        &self,
        ctx: &ActionContext,
        id: i64,
    ) -> Result<Option<Record>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_VIEW_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(rows.into_iter().next())
    }

    pub(super) async fn find_authorization_state_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
    ) -> Result<Option<AuthorizationStateRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(USER_AUTHORIZATION_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        rows.first()
            .map(AuthorizationStateRecord::try_from)
            .transpose()
    }

    pub(super) async fn insert(
        &self,
        ctx: &ActionContext,
        username: &str,
        password_hash: &str,
        email: &str,
        email_verified_at: i64,
    ) -> Result<i64, BaseError> {
        let record = Record::new()
            .set(USERNAME, username)
            .set(PASSWORD_HASH, password_hash)
            .set(EMAIL, email)
            .set(EMAIL_VERIFIED_AT, email_verified_at)
            .set(STATUS, UserStatus::Active.as_str());
        let (_, id) = self.trusted_query(ctx)?.insert_returning_id(record).await?;
        i64::try_from(id).map_err(|_| BaseError::Unknown("用户主键超出 i64 范围".to_string()))
    }

    pub(super) async fn update_password_hash_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        id: i64,
        password_hash: &str,
    ) -> Result<(), BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(id.into()))?
            .update_in_tx(transaction, Record::new().set(PASSWORD_HASH, password_hash))
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                format!("用户 {id} 密码摘要更新未精确影响一行"),
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::account::user::schema::user_table_spec;
    use sqlx::mysql::MySqlPoolOptions;
    use yang_base::action::Request;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    #[tokio::test]
    async fn user_repository_owns_the_only_trusted_password_projection() {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let mysql = Database::from_pool(pool, DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        let definition = user_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("用户表定义应有效: {error}"));
        let repository = UserRepository::new(definition.clone());
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), tools)
            .with_table_definition(definition);

        assert!(repository
            .trusted_query(&ctx)
            .and_then(|query| {
                query.select_fields(&[PASSWORD_HASH, AUTHZ_VERSION, CREDENTIAL_VERSION])
            })
            .is_ok());
        for field_name in [PASSWORD_HASH, AUTHZ_VERSION, CREDENTIAL_VERSION] {
            assert!(matches!(
                ctx.table_query()
                    .and_then(|query| query.select_fields(&[field_name])),
                Err(BaseError::FieldPermissionDenied(_, field, _)) if field == field_name
            ));
        }
    }
}
