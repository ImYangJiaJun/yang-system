//! 授权事实表 `authz_grant` 的唯一持久化边界。
//! authorization-writer: access-grant-lifecycle
//!
//! 对外 `TableQuery` 始终以 `system` 能力运行：授权事实不暴露给字段权限体系，
//! 读取（Token 签发快照、管理查询）与写入（授权/撤销）都收敛在本 Repository。

use crate::addon::access::grants::table::{
    EXPIRES_AT, GRANTED_BY, GRANT_ID, GRANT_RECORD_FIELDS, OCCURRED_AT, PERMISSION, SYSTEM_ROLE,
    USER_ID,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery, WhereCondition};
use yang_base::BaseError;

/// 一条直授权限事实；`expires_at` 为 `None` 表示永久有效（NULL=永久）。
/// `user_id` 随行读出，批量判据（一次读多用户）与单用户查询共用同一结构。
pub(crate) struct GrantRecord {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) permission: String,
    pub(crate) granted_by: i64,
    pub(crate) occurred_at: i64,
    pub(crate) expires_at: Option<i64>,
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
            expires_at: record.optional(EXPIRES_AT)?,
        })
    }
}

/// 判定一条直授事实在 `now` 时刻是否已过期。
///
/// `None`（NULL）= 永久有效，永不过期；`Some(expires_at) <= now` 视为已过期
/// （边界取等号：`expires_at` 当刻即失效，与 SQL 过滤的 `expires_at <= now` 同源）。
pub(crate) fn is_expired(expires_at: Option<i64>, now: i64) -> bool {
    expires_at.is_some_and(|expires| expires <= now)
}

/// 当前 Unix 秒（直授过期判定与写入的时间基准，全模块共用一处实现）。
pub(crate) fn current_unix_timestamp() -> Result<i64, BaseError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BaseError::ConfigError("系统时间早于 Unix epoch".to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| BaseError::ConfigError("系统时间超出 i64 范围".to_string()))
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

    /// 读取目标用户的**未过期**直授权限（Token 签发快照、提权判定与管理查询共享），
    /// 按主键稳定排序。
    ///
    /// 过期过滤下推到 SQL（`expires_at IS NULL OR expires_at > now`）：过期行按审计
    /// 要求**保留**在表里，只在读取侧失效；需要连过期行一起看时用
    /// [`Self::list_all_by_user_in_tx`]。
    pub(crate) async fn list_by_user_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
    ) -> Result<Vec<GrantRecord>, BaseError> {
        let now = current_unix_timestamp()?;
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GRANT_RECORD_FIELDS)?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_or(vec![
                WhereCondition::IsNull {
                    field: EXPIRES_AT.to_string(),
                },
                WhereCondition::Gt {
                    field: EXPIRES_AT.to_string(),
                    value: serde_json::Value::Number(now.into()),
                },
            ])?
            .all_in_tx(transaction)
            .await?;
        rows.iter().map(GrantRecord::try_from).collect()
    }

    /// 读取目标用户的**全部**直授权限（含过期行，审计展示用），按主键稳定排序。
    ///
    /// 过期行保留做审计：`list_user_grants` 用它展示 `expired` 派生标记，
    /// 不会像解析侧那样把过期行静默过滤掉。
    pub(crate) async fn list_all_by_user_in_tx(
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

    /// 读取持有目标权限的**全部**直授行（含过期行，权限下钻的审计口径），按主键稳定排序。
    ///
    /// 与 [`Self::list_all_by_user_in_tx`] 同为审计读端：过期行保留在表里只在解析侧
    /// 失效，下钻视图要把它们连 `expired` 派生标记一起展示，不能像解析侧那样过滤掉。
    /// 过滤下推到 `permission` 列，命中表声明里的反查索引 `idx_authz_grant_permission`。
    pub(crate) async fn list_by_permission_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        permission: &str,
    ) -> Result<Vec<GrantRecord>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GRANT_RECORD_FIELDS)?
            .where_eq(
                PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .all_in_tx(transaction)
            .await?;
        rows.iter().map(GrantRecord::try_from).collect()
    }

    /// 读取一批用户的**全部**直授行（含过期行），批量授予/撤销的一次性判据读。
    ///
    /// 调用方必须已在同一事务按全局锁序锁住这批用户的 users 行（判据读晚于行锁），
    /// 且 `user_ids` 非空。
    pub(crate) async fn list_by_users_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_ids: &[i64],
    ) -> Result<Vec<GrantRecord>, BaseError> {
        let values = user_ids
            .iter()
            .map(|user_id| serde_json::Value::Number((*user_id).into()))
            .collect();
        let rows = self
            .trusted_query(ctx)?
            .select_fields(GRANT_RECORD_FIELDS)?
            .where_in(USER_ID, values)?
            .all_in_tx(transaction)
            .await?;
        rows.iter().map(GrantRecord::try_from).collect()
    }

    /// 查询 (user_id, permission) 既有直授行的过期时间。
    ///
    /// 无行返回 `None`；有行返回 `Some(expires_at)`（`None` 表示该行永久有效）。
    /// 与旧 `exists_in_tx` 的区别是同时带回过期信息，供「跳过 vs 续期」决策。
    pub(crate) async fn find_expiry_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
        permission: &str,
    ) -> Result<Option<Option<i64>>, BaseError> {
        let rows = self
            .trusted_query(ctx)?
            .select_fields(&[GRANT_ID, EXPIRES_AT])?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(
                PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .page(1, 1)?
            .all_in_tx(transaction)
            .await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(row.optional(EXPIRES_AT)?))
    }

    /// 写入一条直授权限事实；调用方必须已在同事务持有目标用户行锁。
    pub(crate) async fn insert_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
        permission: &str,
        granted_by: i64,
        expires_at: Option<i64>,
    ) -> Result<(), BaseError> {
        let mut record = Record::new()
            .set(USER_ID, user_id)
            .set(PERMISSION, permission)
            .set(GRANTED_BY, granted_by);
        if let Some(expires_at) = expires_at {
            record = record.set(EXPIRES_AT, expires_at);
        }
        self.trusted_query(ctx)?
            .insert_in_tx(transaction, record)
            .await?;
        Ok(())
    }

    /// 续期一条已过期的直授事实：原地改写过期时间与授权操作人，行保留做审计。
    ///
    /// 唯一键 `(user_id, permission)` 不允许同一权限插第二行，过期行又不清理，
    /// 因此重新授予过期权限必须走 UPDATE 而不是 INSERT——`occurred_at` 保持首次
    /// 授权时间不变，`expires_at` 置 `None` 即续为永久。
    /// 调用方必须已在同事务持有目标用户行锁。
    pub(crate) async fn renew_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
        permission: &str,
        granted_by: i64,
        expires_at: Option<i64>,
    ) -> Result<(), BaseError> {
        let record = Record::new()
            .set(GRANTED_BY, granted_by)
            .set(EXPIRES_AT, serde_json::Value::Null);
        let record = if let Some(expires_at) = expires_at {
            record.set(EXPIRES_AT, expires_at)
        } else {
            record
        };
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .where_eq(
                PERMISSION,
                serde_json::Value::String(permission.to_string()),
            )?
            .update_in_tx(transaction, record)
            .await?;
        if affected != 1 {
            return Err(BaseError::from(yang_db::DbError::TransactionError(
                "续期直授行不存在或意外变化".to_string(),
            )));
        }
        Ok(())
    }

    /// 删除目标用户的**全部**直授权限，返回删除行数（账号删除时的孤儿清理）。
    ///
    /// 账号删除后直授事实已无意义，必须一次清干净，否则留下指向已匿名化账号的
    /// 悬空行（spec §8.3）。与 [`Self::delete_in_tx`] 的区别是不指定权限字符串。
    pub(crate) async fn delete_all_of_user_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut yang_db::Transaction,
        user_id: i64,
    ) -> Result<u64, BaseError> {
        let affected = self
            .trusted_query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }

    /// 删除一条直授权限事实，返回影响行数（0 表示目标用户本就没有该权限）。
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
        assert_eq!(grant.id, 1);
        assert_eq!(grant.permission, "access.grants.read");
        assert_eq!(grant.expires_at, None, "未显式写入 expires_at 时按永久读取");

        let incomplete = Record::new().set(GRANT_ID, 1_i64);
        assert!(GrantRecord::try_from(&incomplete).is_err());
    }

    #[test]
    fn grant_record_reads_nullable_expiry() {
        let expired = Record::new()
            .set(GRANT_ID, 1_i64)
            .set(USER_ID, 7_i64)
            .set(PERMISSION, "access.grants.read")
            .set(GRANTED_BY, 9_i64)
            .set(OCCURRED_AT, 1_700_000_000_i64)
            .set(EXPIRES_AT, 1_800_000_000_i64);
        let grant = GrantRecord::try_from(&expired)
            .unwrap_or_else(|error| panic!("带过期时间的记录应转换为授权事实: {error}"));
        assert_eq!(grant.user_id, 7);
        assert_eq!(grant.expires_at, Some(1_800_000_000));

        let permanent = Record::new()
            .set(GRANT_ID, 2_i64)
            .set(USER_ID, 7_i64)
            .set(PERMISSION, "access.grants.write")
            .set(GRANTED_BY, 9_i64)
            .set(OCCURRED_AT, 1_700_000_000_i64)
            .set(EXPIRES_AT, serde_json::Value::Null);
        let grant = GrantRecord::try_from(&permanent)
            .unwrap_or_else(|error| panic!("NULL 过期时间应读为永久: {error}"));
        assert_eq!(grant.expires_at, None);
    }

    #[tokio::test]
    async fn expired_rows_are_excluded_from_resolution_by_sql_filter() {
        // 解析侧（list_by_user_in_tx）的过期过滤必须下推到 SQL：
        // `expires_at IS NULL OR expires_at > now`，过期行保留在表里只做读取侧失效。
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition = grants_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let repository = GrantRepository::new(definition.clone());
        let ctx = ActionContext::new(
            Request::new(serde_json::json!({})),
            Arc::new(
                ToolsBuilder::new()
                    .mysql(
                        Database::from_pool(pool, DatabaseConfig::default())
                            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}")),
                    )
                    .build()
                    .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
            ),
        )
        .with_table_definition(definition);

        let query = repository
            .trusted_query(&ctx)
            .and_then(|query| query.select_fields(GRANT_RECORD_FIELDS))
            .and_then(|query| query.where_eq(USER_ID, serde_json::Value::Number(7_i64.into())))
            .and_then(|query| {
                query.where_or(vec![
                    WhereCondition::IsNull {
                        field: EXPIRES_AT.to_string(),
                    },
                    WhereCondition::Gt {
                        field: EXPIRES_AT.to_string(),
                        value: serde_json::Value::Number(now_in_test().into()),
                    },
                ])
            })
            .unwrap_or_else(|error| panic!("过期过滤查询应可构建: {error}"));

        let conditions = &query.get_query_params().where_conditions;
        let or_group = conditions.iter().find_map(|condition| match condition {
            WhereCondition::Or { conditions } => Some(conditions),
            _ => None,
        });
        let or_group = or_group.unwrap_or_else(|| panic!("过期过滤必须是 OR 布尔组"));
        assert_eq!(or_group.len(), 2, "NULL=永久 与 expires_at > now 两个分支");
        assert!(matches!(
            or_group[0],
            WhereCondition::IsNull { ref field } if field == EXPIRES_AT
        ));
        assert!(matches!(
            or_group[1],
            WhereCondition::Gt { ref field, .. } if field == EXPIRES_AT
        ));
    }

    #[tokio::test]
    async fn by_permission_query_targets_the_permission_column() {
        // list_by_permission_in_tx 的过滤必须落在 permission 列上：命中反查索引
        // `idx_authz_grant_permission`，而不是只靠 (user_id, permission) 复合键前缀。
        let pool = MySqlPoolOptions::new()
            .connect_lazy("mysql://root:test@127.0.0.1:3306/test")
            .unwrap_or_else(|error| panic!("测试连接配置应有效: {error}"));
        let definition = grants_table_spec()
            .and_then(|spec| spec.table_definition())
            .unwrap_or_else(|error| panic!("授权表定义应有效: {error}"));
        let repository = GrantRepository::new(definition.clone());
        let ctx = ActionContext::new(
            Request::new(serde_json::json!({})),
            Arc::new(
                ToolsBuilder::new()
                    .mysql(
                        Database::from_pool(pool, DatabaseConfig::default())
                            .unwrap_or_else(|error| panic!("测试 Database 应构建成功: {error}")),
                    )
                    .build()
                    .unwrap_or_else(|error| panic!("测试 Tools 应构建成功: {error}")),
            ),
        )
        .with_table_definition(definition);

        let query = repository
            .trusted_query(&ctx)
            .and_then(|query| query.select_fields(GRANT_RECORD_FIELDS))
            .and_then(|query| {
                query.where_eq(
                    PERMISSION,
                    serde_json::Value::String("access.grants.read".to_string()),
                )
            })
            .unwrap_or_else(|error| panic!("按权限过滤查询应可构建: {error}"));

        let conditions = &query.get_query_params().where_conditions;
        assert_eq!(
            conditions.len(),
            1,
            "只按 permission 等值过滤（不夹过期条件）"
        );
        assert!(matches!(
            &conditions[0],
            WhereCondition::Eq { field, value } if field == PERMISSION && value == &serde_json::json!("access.grants.read")
        ));
    }

    fn now_in_test() -> i64 {
        1_700_000_000
    }

    #[test]
    fn expiry_judgement_covers_past_future_and_permanent() {
        let now = 1_700_000_000;
        assert!(is_expired(Some(now - 1), now), "过去时刻必须已过期");
        assert!(
            is_expired(Some(now), now),
            "等于 now 当刻即失效（边界含等号）"
        );
        assert!(!is_expired(Some(now + 1), now), "未来时刻未过期");
        assert!(!is_expired(None, now), "NULL=永久，永不过期");
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
        for field_name in [USER_ID, PERMISSION, GRANTED_BY, OCCURRED_AT, EXPIRES_AT] {
            assert!(matches!(
                ctx.table_query()
                    .and_then(|query| query.select_fields(&[field_name])),
                Err(BaseError::FieldPermissionDenied(_, field, _)) if field == field_name
            ));
        }
    }
}
