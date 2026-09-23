//! 权限组事实（组、条目、成员）的唯一持久化边界。
//! authorization-writer: access-group-lifecycle
//!
//! 对外 `TableQuery` 始终以 `system` 能力运行：组事实不暴露给字段权限体系，
//! 读取（解析、管理查询）与写入（组生命周期）都收敛在本 Repository。

// 本任务先定型组事实的受信 writer 接口，消费者（有效权限解析在 Task 4、
// 管理编排与引导在 Task 6/8、管理 Action 在 Task 10+）晚于本任务接入，
// 与 `tables.rs` / `groups/table.rs` 同例显式豁免 dead-code 门禁。
#![allow(dead_code)]

use crate::addon::access::domain::groups::tables::{
    ITEM_GRANTED_BY, ITEM_GROUP_ID, ITEM_PERMISSION, MEMBER_GRANTED_BY, MEMBER_GROUP_ID,
    MEMBER_USER_ID, OWNER_SENTINEL_KEY, OWNER_USER_ID, SENTINEL_KEY_VALUE, SYSTEM_ROLE,
};
use crate::addon::access::groups::table::{
    GROUP_CREATED_BY, GROUP_DESCRIPTION, GROUP_ID, GROUP_KEY, GROUP_RECORD_FIELDS, GROUP_TITLE,
};
use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;
use yang_db::Transaction;

/// 内置全权组的固定标识：引导首个账号时幂等创建。
pub(crate) const SYSTEM_ADMIN_GROUP_KEY: &str = "system_admin";

/// 单个权限组的成员上限（管理侧准入与解析侧共用同一口径）。
pub(crate) const MAX_GROUP_MEMBERS: usize = 200;

/// 一条权限组事实。
pub(crate) struct GroupRecord {
    pub(crate) id: i64,
    pub(crate) group_key: String,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    pub(crate) created_by: i64,
}

impl TryFrom<&Record> for GroupRecord {
    type Error = BaseError;

    fn try_from(record: &Record) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.require(GROUP_ID)?,
            group_key: record.require(GROUP_KEY)?,
            title: record.require(GROUP_TITLE)?,
            description: record.optional(GROUP_DESCRIPTION)?,
            created_by: record.require(GROUP_CREATED_BY)?,
        })
    }
}

/// 权限组事实（组、条目、成员）的唯一受信持久化边界。
pub(crate) struct GroupRepository {
    groups: TableDefinition,
    items: TableDefinition,
    members: TableDefinition,
    /// 引导哨兵表：Task 8 的系统管理员声明写入经同一受信边界完成，
    /// 因此表定义在此一次性定型，避免后续改动构造签名。
    owners: TableDefinition,
}

impl GroupRepository {
    pub(crate) fn new(
        groups: TableDefinition,
        items: TableDefinition,
        members: TableDefinition,
        owners: TableDefinition,
    ) -> Self {
        Self {
            groups,
            items,
            members,
            owners,
        }
    }

    /// 组事实的读端一律以 `system` 能力运行，不受字段权限体系限制。
    fn trusted(definition: &TableDefinition, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(definition.bind(pool).query([SYSTEM_ROLE]))
    }

    fn trusted_query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        Self::trusted(&self.groups, ctx)
    }

    fn trusted_items(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        Self::trusted(&self.items, ctx)
    }

    fn trusted_members(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        Self::trusted(&self.members, ctx)
    }

    fn trusted_owner(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        Self::trusted(&self.owners, ctx)
    }

    /// 幂等地确保内置全权组存在，返回其 id。
    ///
    /// 并发下两个调用者可能同时插入：唯一键冲突按「别人已建好」处理，
    /// 回查后返回既有 id（savepoint-and-refetch 模式）。
    pub(crate) async fn ensure_system_admin_group_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
    ) -> Result<i64, BaseError> {
        if let Some(existing) = self
            .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
            .await?
        {
            return Ok(existing.id);
        }
        match self
            .insert_group_in_tx(
                ctx,
                transaction,
                SYSTEM_ADMIN_GROUP_KEY,
                "系统管理员",
                None,
                0,
            )
            .await
        {
            Ok(id) => Ok(id),
            Err(BaseError::DatabaseExecuteFailed(yang_db::DbError::ConstraintError(_))) => self
                .find_by_key_in_tx(ctx, transaction, SYSTEM_ADMIN_GROUP_KEY)
                .await?
                .map(|record| record.id)
                .ok_or_else(|| BaseError::ConfigError("内置权限组插入冲突后回查不到".to_string())),
            Err(error) => Err(error),
        }
    }

    /// 竞争引导哨兵；重复插入由唯一约束与 CHECK 拒绝。
    ///
    /// 这是哨兵表的唯一写入口：并发仲裁完全交给数据库约束，
    /// 调用方不得先「判空」再插入（那是 TOCTOU）。
    pub(crate) async fn insert_owner_sentinel_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError> {
        let record = Record::new()
            .set(OWNER_SENTINEL_KEY, SENTINEL_KEY_VALUE)
            .set(OWNER_USER_ID, user_id);
        self.trusted_owner(ctx)?
            .insert_in_tx(transaction, record)
            .await?;
        Ok(())
    }

    /// 写入一条组事实并返回自增主键。
    pub(crate) async fn insert_group_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_key: &str,
        title: &str,
        description: Option<&str>,
        created_by: i64,
    ) -> Result<i64, BaseError> {
        let record = Record::new()
            .set(GROUP_KEY, group_key)
            .set(GROUP_TITLE, title)
            .set(GROUP_DESCRIPTION, description)
            .set(GROUP_CREATED_BY, created_by);
        let (_, id) = self
            .trusted_query(ctx)?
            .insert_returning_id_in_tx(transaction, record)
            .await?;
        Ok(id as i64)
    }

    /// 按组标识读取组事实。
    pub(crate) async fn find_by_key_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_key: &str,
    ) -> Result<Option<GroupRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GROUP_RECORD_FIELDS)?
            .where_eq(GROUP_KEY, serde_json::Value::String(group_key.to_string()))?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        rows.first().map(GroupRecord::try_from).transpose()
    }

    /// 按主键读取组事实。
    pub(crate) async fn find_by_id_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
    ) -> Result<Option<GroupRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GROUP_RECORD_FIELDS)?
            .where_eq(GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        rows.first().map(GroupRecord::try_from).transpose()
    }

    /// 读取全部组事实（管理列表）。
    pub(crate) async fn list_groups_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
    ) -> Result<Vec<GroupRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GROUP_RECORD_FIELDS)?
            .all_in_tx(transaction)
            .await?;
        rows.iter().map(GroupRecord::try_from).collect()
    }

    /// 删除一条组事实，返回影响行数（0 表示组不存在）。
    ///
    /// 仍有成员时数据库按外键 RESTRICT 拒绝删除，错误原样上抛。
    pub(crate) async fn delete_group_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
    ) -> Result<u64, BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }

    /// 删除一个组的全部权限条目，返回删除行数。
    ///
    /// 条目表只有唯一键与 CHECK、没有到 `permission_group` 的外键，删组时数据库
    /// 不会替我们清理，因此这里必须显式删——否则会留下悬空条目行。
    pub(crate) async fn delete_items_of_group_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
    ) -> Result<u64, BaseError> {
        let affected = self
            .trusted_items(ctx)?
            .where_eq(ITEM_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }

    /// 改写组事实的展示字段，返回影响行数（0 表示组不存在）。
    pub(crate) async fn update_group_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
        title: &str,
        description: Option<&str>,
    ) -> Result<u64, BaseError> {
        let record = Record::new()
            .set(GROUP_TITLE, title)
            .set(GROUP_DESCRIPTION, description);
        let affected = self
            .trusted_query(ctx)?
            .where_eq(GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .update_in_tx(transaction, record)
            .await?;
        Ok(affected)
    }

    /// 追加一条组条目；已存在同一 (组, 权限) 事实时返回 `false`。
    ///
    /// 存在性先查后写：并发下同键插入会以唯一键冲突报错，而不是静默重复，
    /// 调用方已在同事务持有目标组的写路径（管理编排），因此不做错误吞并。
    pub(crate) async fn insert_item_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
        permission: &str,
        granted_by: i64,
    ) -> Result<bool, BaseError> {
        let existing = self
            .trusted_items(ctx)?
            .select_fields(&[ITEM_GROUP_ID])?
            .where_eq(ITEM_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .where_eq(
                ITEM_PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        if !existing.is_empty() {
            return Ok(false);
        }
        let record = Record::new()
            .set(ITEM_GROUP_ID, group_id)
            .set(ITEM_PERMISSION, permission)
            .set(ITEM_GRANTED_BY, granted_by);
        self.trusted_items(ctx)?
            .insert_in_tx(transaction, record)
            .await?;
        Ok(true)
    }

    /// 删除一条组条目，返回是否真的删除了事实。
    pub(crate) async fn delete_item_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
        permission: &str,
    ) -> Result<bool, BaseError> {
        let affected = self
            .trusted_items(ctx)?
            .where_eq(ITEM_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .where_eq(
                ITEM_PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected > 0)
    }

    /// 读取一个组的全部权限条目。
    pub(crate) async fn list_items_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
    ) -> Result<Vec<String>, BaseError> {
        let rows = self
            .trusted_items(ctx)?
            .select_fields(&[ITEM_PERMISSION])?
            .where_eq(ITEM_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .all_in_tx(transaction)
            .await?;
        rows.iter()
            .map(|record| record.require(ITEM_PERMISSION))
            .collect()
    }

    /// 追加一条用户-组关系；该用户已在此组时返回 `false`。
    pub(crate) async fn insert_member_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        group_id: i64,
        granted_by: i64,
    ) -> Result<bool, BaseError> {
        let existing = self
            .trusted_members(ctx)?
            .select_fields(&[MEMBER_USER_ID])?
            .where_eq(MEMBER_USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(MEMBER_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        if !existing.is_empty() {
            return Ok(false);
        }
        let record = Record::new()
            .set(MEMBER_USER_ID, user_id)
            .set(MEMBER_GROUP_ID, group_id)
            .set(MEMBER_GRANTED_BY, granted_by);
        self.trusted_members(ctx)?
            .insert_in_tx(transaction, record)
            .await?;
        Ok(true)
    }

    /// 删除一条用户-组关系，返回是否真的删除了事实。
    pub(crate) async fn delete_member_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
        group_id: i64,
    ) -> Result<bool, BaseError> {
        let affected = self
            .trusted_members(ctx)?
            .where_eq(MEMBER_USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(MEMBER_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected > 0)
    }

    /// 读取一个组的成员用户 id，按 `user_id` 升序。
    ///
    /// 升序是扇出失效的锁序契约：受影响用户必须按同一顺序加锁，否则两个
    /// 并发变更会互相死锁。
    ///
    /// 排序在内存里做而不是 `ORDER BY`：`user_group.user_id` 只声明了可筛选、
    /// 未声明可排序，走查询层排序会被字段权限门禁拒绝（Task 10 的集成测试正是
    /// 撞在这里）。这里需要的只是确定的加锁顺序，而成员集合本来就被
    /// `MAX_GROUP_MEMBERS` 约束成有界规模。
    pub(crate) async fn list_members_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
    ) -> Result<Vec<i64>, BaseError> {
        let rows = self
            .trusted_members(ctx)?
            .select_fields(&[MEMBER_USER_ID])?
            .where_eq(MEMBER_GROUP_ID, serde_json::Value::Number(group_id.into()))?
            .all_in_tx(transaction)
            .await?;
        let mut members: Vec<i64> = rows
            .iter()
            .map(|record| record.require(MEMBER_USER_ID))
            .collect::<Result<Vec<i64>, BaseError>>()?;
        members.sort_unstable();
        Ok(members)
    }

    /// 读取一个用户所属的全部组 id。
    pub(crate) async fn list_group_ids_of_user_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<Vec<i64>, BaseError> {
        let rows = self
            .trusted_members(ctx)?
            .select_fields(&[MEMBER_GROUP_ID])?
            .where_eq(MEMBER_USER_ID, serde_json::Value::Number(user_id.into()))?
            .all_in_tx(transaction)
            .await?;
        rows.iter()
            .map(|record| record.require(MEMBER_GROUP_ID))
            .collect()
    }

    /// 统计一个组的成员数。
    pub(crate) async fn count_members_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        group_id: i64,
    ) -> Result<u64, BaseError> {
        // TableQuery 只提供事务外 COUNT；成员数受 MAX_GROUP_MEMBERS 上限约束，
        // 读回成员主键后计数与其他读端同源，不为此引入原始 SQL。
        let members = self.list_members_in_tx(ctx, transaction, group_id).await?;
        Ok(members.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::access::groups::table::{
        GROUP_CREATED_BY, GROUP_DESCRIPTION, GROUP_ID, GROUP_KEY, GROUP_TITLE,
    };
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use yang_base::table::{Record, TableDefinition};
    use yang_base::{action::ActionContext, action::Request, tools::ToolsBuilder};
    use yang_db::{Database, DatabaseConfig};

    fn lazy_pool() -> Arc<sqlx::MySqlPool> {
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        Arc::new(pool)
    }

    fn test_context() -> ActionContext {
        let mysql = Database::from_pool((*lazy_pool()).clone(), DatabaseConfig::default())
            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}"));
        let tools = Arc::new(
            ToolsBuilder::new()
                .mysql(mysql)
                .build()
                .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
        );
        ActionContext::new(Request::new(serde_json::json!({})), tools)
    }

    fn test_repository() -> GroupRepository {
        let definition =
            |spec: Result<yang_base::definition::TableSpec, BaseError>| -> TableDefinition {
                spec.and_then(|spec| spec.table_definition())
                    .unwrap_or_else(|error| panic!("表定义应有效: {error}"))
            };
        GroupRepository::new(
            definition(crate::addon::access::groups::table::groups_table_spec()),
            definition(super::super::tables::group_items_table_spec()),
            definition(super::super::tables::user_group_table_spec()),
            definition(super::super::tables::system_owner_table_spec()),
        )
    }

    // 懒连接池的构建需要 Tokio 上下文，与 grants/table.rs 的既有样例同例。
    #[tokio::test]
    async fn group_facts_are_only_readable_by_the_system_capability() {
        let groups = crate::addon::access::groups::table::groups_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("组表定义应有效: {error}"));
        let table = groups.bind(lazy_pool());

        let denied = table
            .query(["user"])
            .select_fields(&[crate::addon::access::groups::table::GROUP_KEY]);
        assert!(
            matches!(denied, Err(BaseError::FieldPermissionDenied(_, field, _)) if field == crate::addon::access::groups::table::GROUP_KEY),
            "组事实必须只对 system 能力可读"
        );
        assert!(table
            .query([crate::addon::access::groups::table::SYSTEM_ROLE])
            .select_fields(&[crate::addon::access::groups::table::GROUP_KEY])
            .is_ok());
    }

    #[tokio::test]
    async fn group_repository_exposes_a_trusted_query() {
        let repository = test_repository();
        let ctx = test_context();
        assert!(repository.trusted_query(&ctx).is_ok());
    }

    #[test]
    fn group_record_requires_a_complete_row() {
        let record = Record::new()
            .set(GROUP_ID, 1_i64)
            .set(GROUP_KEY, "team_admins")
            .set(GROUP_TITLE, "团队管理员")
            .set(GROUP_DESCRIPTION, "负责团队内的权限分配")
            .set(GROUP_CREATED_BY, 9_i64);
        let group = GroupRecord::try_from(&record)
            .unwrap_or_else(|error| panic!("完整记录应转换为组事实: {error}"));
        assert_eq!(group.id, 1);
        assert_eq!(group.group_key, "team_admins");
        assert_eq!(group.title, "团队管理员");
        assert_eq!(group.description.as_deref(), Some("负责团队内的权限分配"));
        assert_eq!(group.created_by, 9);

        // 可空列缺失时必须映射为 None，而不是把整行判为非法。
        let without_description = Record::new()
            .set(GROUP_ID, 2_i64)
            .set(GROUP_KEY, "readonly")
            .set(GROUP_TITLE, "只读")
            .set(GROUP_CREATED_BY, 9_i64);
        let group = GroupRecord::try_from(&without_description)
            .unwrap_or_else(|error| panic!("缺 description 的记录仍应合法: {error}"));
        assert_eq!(group.description, None);

        let incomplete = Record::new().set(GROUP_ID, 1_i64);
        assert!(GroupRecord::try_from(&incomplete).is_err());
    }

    #[test]
    fn max_group_members_is_a_named_constant() {
        assert_eq!(MAX_GROUP_MEMBERS, 200);
        assert_eq!(SYSTEM_ADMIN_GROUP_KEY, "system_admin");
    }
}
