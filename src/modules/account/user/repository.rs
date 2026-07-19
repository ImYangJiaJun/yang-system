//! 用户凭据的受信持久化边界。
//!
//! 对外 `TableQuery` 始终遵守请求用户的字段权限；只有本 Repository 能以
//! `system` 能力读写密码摘要，避免公开注册和登录被字段权限拦截，也避免把通用
//! 提权查询暴露给 Action。

use super::schema::{PASSWORD_HASH, STATUS, SYSTEM_ROLE, USERNAME, USER_ID};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

const USER_CREDENTIAL_FIELDS: &[&str] = &[USER_ID, USERNAME, PASSWORD_HASH, STATUS];

pub(super) struct CredentialRecord {
    pub(super) id: i64,
    pub(super) username: String,
    pub(super) password_hash: String,
    pub(super) status: String,
}

impl TryFrom<&Record> for CredentialRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(USER_ID)?,
            username: record.require(USERNAME)?,
            password_hash: record.require(PASSWORD_HASH)?,
            status: record.require(STATUS)?,
        })
    }
}

pub(super) struct CredentialRepository {
    users: TableDefinition,
}

impl CredentialRepository {
    pub(super) fn new(users: TableDefinition) -> Self {
        Self { users }
    }

    fn query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.users.bind(pool).query([SYSTEM_ROLE]))
    }

    pub(super) async fn username_exists(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(USERNAME, serde_json::Value::String(username.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        Ok(!rows.is_empty())
    }

    pub(super) async fn find_by_username(
        &self,
        ctx: &ActionContext,
        username: &str,
    ) -> Result<Option<CredentialRecord>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(USER_CREDENTIAL_FIELDS)?
            .where_eq(USERNAME, serde_json::Value::String(username.to_string()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(CredentialRecord::try_from).transpose()
    }

    pub(super) async fn insert(
        &self,
        ctx: &ActionContext,
        username: &str,
        password_hash: &str,
    ) -> Result<i64, BaseError> {
        let record = Record::new()
            .set(USERNAME, username)
            .set(PASSWORD_HASH, password_hash)
            .set(STATUS, "active");
        let (_, id) = self.query(ctx)?.insert_returning_id(record).await?;
        i64::try_from(id).map_err(|_| BaseError::Unknown("用户主键超出 i64 范围".to_string()))
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
    async fn credential_repository_owns_the_only_trusted_password_projection() {
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
        let repository = CredentialRepository::new(definition.clone());
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), tools)
            .with_table_definition(definition);

        assert!(repository
            .query(&ctx)
            .and_then(|query| query.select_fields(&[PASSWORD_HASH]))
            .is_ok());
        assert!(matches!(
            ctx.table_query()
                .and_then(|query| query.select_fields(&[PASSWORD_HASH])),
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == PASSWORD_HASH
        ));
    }
}
