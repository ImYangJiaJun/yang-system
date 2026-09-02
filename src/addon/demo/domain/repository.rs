//! 便签事实表 `demo_note` 的唯一持久化边界。
//!
//! 所有权规则在此收敛：写路径的 `owner_user_id` 永远来自已认证操作者，
//! 读/改/删都强制携带 `owner_user_id = 当前用户` 条件，任何调用方都无法
//! 跨用户访问便签。对外 `TableQuery` 以 `system` 能力运行（字段权限体系
//! 已拒绝普通角色写入归属人字段）。

use crate::addon::demo::notes::table::{CONTENT, NOTE_ID, OWNER_USER_ID, SYSTEM_ROLE, TITLE};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;

pub(crate) struct NoteRepository {
    notes: TableDefinition,
}

impl NoteRepository {
    pub(crate) fn new(notes: TableDefinition) -> Self {
        Self { notes }
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.notes.bind(pool).query([SYSTEM_ROLE]))
    }

    /// 创建一条归属当前用户的便签，返回自增主键。
    pub(crate) async fn insert_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        owner_user_id: i64,
        title: &str,
        content: Option<&str>,
    ) -> Result<i64, BaseError> {
        let mut record = Record::new()
            .set(OWNER_USER_ID, owner_user_id)
            .set(TITLE, title);
        if let Some(content) = content {
            record = record.set(CONTENT, content);
        }
        let (_, id) = self
            .trusted_query(ctx)?
            .insert_returning_id_in_tx(transaction, record)
            .await?;
        i64::try_from(id)
            .map_err(|error| BaseError::ConfigError(format!("便签自增主键超出 i64 范围: {error}")))
    }

    /// 更新一条便签；WHERE 同时携带主键与归属人，跨用户目标影响 0 行。
    pub(crate) async fn update_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        note_id: i64,
        owner_user_id: i64,
        title: Option<&str>,
        content: Option<&str>,
    ) -> Result<u64, BaseError> {
        let mut record = Record::new();
        if let Some(title) = title {
            record = record.set(TITLE, title);
        }
        if let Some(content) = content {
            record = record.set(CONTENT, content);
        }
        self.trusted_query(ctx)?
            .where_eq(NOTE_ID, serde_json::Value::Number(note_id.into()))?
            .where_eq(
                OWNER_USER_ID,
                serde_json::Value::Number(owner_user_id.into()),
            )?
            .update_in_tx(transaction, record)
            .await
    }

    /// 删除一条便签；WHERE 同时携带主键与归属人，返回影响行数。
    pub(crate) async fn delete_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        note_id: i64,
        owner_user_id: i64,
    ) -> Result<u64, BaseError> {
        self.trusted_query(ctx)?
            .where_eq(NOTE_ID, serde_json::Value::Number(note_id.into()))?
            .where_eq(
                OWNER_USER_ID,
                serde_json::Value::Number(owner_user_id.into()),
            )?
            .delete_in_tx(transaction)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::demo::notes::table::notes_table_spec;
    use sqlx::mysql::MySqlPoolOptions;
    use yang_base::action::Request;
    use yang_base::tools::ToolsBuilder;
    use yang_db::{Database, DatabaseConfig};

    #[tokio::test]
    async fn note_repository_owns_the_only_trusted_write_projection() {
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
        let definition = notes_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("便签表定义应有效: {error}"));
        let repository = NoteRepository::new(definition.clone());
        let ctx = ActionContext::new(Request::new(serde_json::json!({})), tools)
            .with_table_definition(definition);

        // 受信 writer 可以构造所有权 scoped 的更新（不执行，只验证权限与条件装配）。
        assert!(repository
            .trusted_query(&ctx)
            .and_then(|query| query
                .where_eq(NOTE_ID, serde_json::Value::Number(1_i64.into()))
                .and_then(
                    |query| query.where_eq(OWNER_USER_ID, serde_json::Value::Number(7_i64.into()))
                ))
            .is_ok());
        // 请求级（普通用户角色）写入口不能伪造归属人字段。
        let user_role_write = ctx
            .table_query()
            .unwrap_or_else(|error| panic!("请求级表查询应可构建: {error}"))
            .insert(Record::new().set(OWNER_USER_ID, 7_i64))
            .await;
        assert!(matches!(
            user_role_write,
            Err(BaseError::FieldPermissionDenied(_, field, _)) if field == OWNER_USER_ID
        ));
        // 请求级查询可以按归属人过滤（列表所有权 scoped 查询依赖此能力）。
        let scoped = ctx.table_query().and_then(|query| {
            query.where_eq(OWNER_USER_ID, serde_json::Value::Number(7_i64.into()))
        });
        assert!(scoped.is_ok());
    }
}
