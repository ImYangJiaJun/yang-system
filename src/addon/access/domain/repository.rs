//! 授权事实表 `authz_grant` 的唯一持久化边界。
//! authorization-writer: access-grant-lifecycle
//!
//! 对外 `TableQuery` 始终以 `system` 能力运行：授权事实不暴露给字段权限体系，
//! 读取（Token 签发快照、管理查询）与写入（授权/撤销）都收敛在本 Repository。

use crate::addon::access::grants::table::{
    GRANTED_BY, GRANT_ID, GRANT_RECORD_FIELDS, OCCURRED_AT, PERMISSION, SYSTEM_ROLE, USER_ID,
};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

/// 一条直授权限事实。
// P3 授权管理 Action 消费全部字段；接入后移除 allow。
#[allow(dead_code)]
pub(crate) struct GrantRecord {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) permission: String,
    pub(crate) granted_by: i64,
    pub(crate) occurred_at: i64,
}

impl TryFrom<&Record> for GrantRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(GRANT_ID)?,
            user_id: record.require(USER_ID)?,
            permission: record.require(PERMISSION)?,
            granted_by: record.require(GRANTED_BY)?,
            occurred_at: record.require(OCCURRED_AT)?,
        })
    }
}

pub(crate) struct GrantRepository {
    grants: TableDefinition,
}

impl GrantRepository {
    pub(crate) fn new(grants: TableDefinition) -> Self {
        Self { grants }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.grants.bind(pool).query([SYSTEM_ROLE]))
    }

    /// 读取目标用户的全部直授权限（Token 签发快照与管理查询共享），按主键稳定排序。
    pub(crate) async fn list_by_user_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
    ) -> Result<Vec<GrantRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GRANT_RECORD_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .all_in_tx(transaction)
            .await?;
        rows.iter().map(GrantRecord::try_from).collect()
    }

    /// 判断目标用户是否已持有某权限的直授事实。
    // P3 授权管理 Action 消费；接入后移除 allow。
    #[allow(dead_code)]
    pub(crate) async fn exists_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
        permission: &str,
    ) -> Result<bool, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[GRANT_ID])?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(
                PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        Ok(!rows.is_empty())
    }

    /// 写入一条直授权限事实；调用方必须已在同事务持有目标用户行锁。
    // P3 授权管理 Action 消费；接入后移除 allow。
    #[allow(dead_code)]
    pub(crate) async fn insert_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
        permission: &str,
        granted_by: i64,
    ) -> Result<(), BaseError> {
        let record = Record::new()
            .set(USER_ID, user_id)
            .set(PERMISSION, permission)
            .set(GRANTED_BY, granted_by);
        self.trusted_query(ctx)?
            .insert_in_tx(transaction, record)
            .await?;
        Ok(())
    }

    /// 删除一条直授权限事实，返回影响行数（0 表示目标用户本就没有该权限）。
    // P3 授权管理 Action 消费；接入后移除 allow。
    #[allow(dead_code)]
    pub(crate) async fn delete_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
        permission: &str,
    ) -> Result<u64, BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(
                PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::access::grants::table::grants_table_spec;
    use sqlx::mysql::MySqlPoolOptions;
    use yang_base::action::Request;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    #[test]
    fn grant_record_requires_a_complete_row() {
        let record = Record::new()
            .set(GRANT_ID, 1_i64)
            .set(USER_ID, 7_i64)
            .set(PERMISSION, "access.grants.read")
            .set(GRANTED_BY, 9_i64)
            .set(OCCURRED_AT, 1_700_000_000_i64);
        let grant = GrantRecord::try_from(&record)
            .unwrap_or_else(|error| panic!("完整记录应转换为授权事实: {error}"));
        assert_eq!(grant.user_id, 7);
        assert_eq!(grant.permission, "access.grants.read");

        let incomplete = Record::new().set(GRANT_ID, 1_i64);
        assert!(GrantRecord::try_from(&incomplete).is_err());
    }

    #[tokio::test]
    async fn grant_repository_owns_the_only_trusted_projection() {
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
        let definition = grants_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let repository = GrantRepository::new(definition.clone());
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), tools)
            .with_table_definition(definition);

        assert!(repository
            .trusted_query(&ctx)
            .and_then(|query| query.select_fields(GRANT_RECORD_FIELDS))
            .is_ok());
        for field_name in [USER_ID, PERMISSION, GRANTED_BY, OCCURRED_AT] {
            assert!(matches!(
                ctx.table_query()
                    .and_then(|query| query.select_fields(&[field_name])),
                Err(BaseError::FieldPermissionDenied(_, field, _)) if field == field_name
            ));
        }
    }
}
