use super::entity::{AccountRow, SYSTEM_ROLE};
use serde_json::Value;
use sqlx::MySqlPool;
use std::collections::HashMap;
use std::sync::Arc;
use yang_base::table::{TableConfig, TableQuery};
use yang_base::BaseError;

#[derive(Clone)]
pub(super) struct AccountRepository {
    pool: Arc<MySqlPool>,
    table: Arc<TableConfig>,
    system_roles: Arc<[String]>,
}

impl AccountRepository {
    pub(super) fn new(pool: Arc<MySqlPool>, table: Arc<TableConfig>) -> Self {
        Self {
            pool,
            table,
            system_roles: Arc::from(vec![SYSTEM_ROLE.to_string()]),
        }
    }

    fn query(&self) -> TableQuery {
        TableQuery::new(
            Arc::clone(&self.table),
            Arc::clone(&self.system_roles),
            Some(Arc::clone(&self.pool)),
        )
    }

    pub(super) async fn find_by_username(
        &self,
        username: &str,
    ) -> Result<Option<AccountRow>, BaseError> {
        let rows = self
            .query()
            .where_eq("username", Value::String(username.to_string()))?
            .select::<AccountRow>()
            .await?;
        Ok(rows.into_iter().next())
    }

    pub(super) async fn find_by_id(&self, id: i64) -> Result<Option<AccountRow>, BaseError> {
        let rows = self
            .query()
            .where_eq("id", Value::Number(id.into()))?
            .select::<AccountRow>()
            .await?;
        Ok(rows.into_iter().next())
    }

    pub(super) async fn insert(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<AccountRow, BaseError> {
        let data = HashMap::from([
            ("username".to_string(), Value::String(username.to_string())),
            (
                "password_hash".to_string(),
                Value::String(password_hash.to_string()),
            ),
            ("status".to_string(), Value::String("active".to_string())),
        ]);
        let (_, id) = match self.query().insert_returning_id(data).await {
            Ok(result) => result,
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => {
                return Err(BaseError::ParamInvalid(
                    "username".to_string(),
                    "用户名已存在".to_string(),
                ));
            }
            Err(error) => return Err(error),
        };
        let id = i64::try_from(id)
            .map_err(|_| BaseError::Unknown("账号主键超出 i64 范围".to_string()))?;
        self.find_by_id(id)
            .await?
            .ok_or_else(|| BaseError::UserNotFound(id.to_string()))
    }
}
