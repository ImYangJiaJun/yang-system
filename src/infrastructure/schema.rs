//! 应用数据库的单一声明式 Schema。
//!
//! 业务表来自 [`yang_base::definition::BuiltApp`]；不进入 UI Catalog 的运行支撑表在
//! 本模块声明。启动时统一预检并同步，不维护版本号、历史 SQL 或 `_migrations` 表。

use std::sync::Arc;
use yang_base::database::{DatabaseInitializer, SchemaSyncReport};
use yang_base::definition::BuiltApp;
use yang_base::table::{Field, Table, TableDefinition};
use yang_base::tools::ToolsBuilder;
use yang_base::BaseError;
use yang_db::{Database, DatabaseConfig};

use crate::config::SecuritySettings;

/// 返回应用所需的完整数据库定义。
pub fn definitions(runtime: &BuiltApp) -> Result<Vec<TableDefinition>, BaseError> {
    let mut definitions = runtime.table_definitions().to_vec();
    definitions.extend(infrastructure_definitions()?);
    Ok(definitions)
}

/// 旧版 `feishu_approval_task` 上的全局唯一索引名。
///
/// 早期版本只对 `record_id` 建唯一键；复制多维表格会原样保留 `record_id`，
/// 副本里的同一记录在另一配置下就会撞键。现行定义是 `(config_id, record_id)`
/// 复合唯一键。
const LEGACY_TASK_RECORD_INDEX: &str = "uk_feishu_approval_task_record_id";

/// 启动前的 fail-closed 门禁：旧库不得残留全局唯一索引
/// `uk_feishu_approval_task_record_id`。
///
/// `schema_sync` 只增不删（见 `crates/yang-base/src/database/schema_sync/plan.rs`），
/// 所以这个索引不会随新定义消失：启动全绿，直到跨配置的同 record_id 记录播种时
/// 撞旧键（只 WARN 跳过），或单条点击在 `ON DUPLICATE` 之后按 `config_id+record_id`
/// 查不到行而报错。
///
/// 发现即**拒绝启动**，不做自动 DROP（DDL 需人工批准）。未命中（含表不存在、
/// 全新库）返回 `Ok`。
pub(crate) async fn ensure_legacy_task_record_index_absent(
    pool: &sqlx::MySqlPool,
) -> Result<(), BaseError> {
    // 经 yang-db 的参数化入口执行：本仓库把直接 `sqlx::query` 收口在显式 raw-sql
    // 边界清单里，本门禁不属于那几类（见 docs/architecture/raw-sql-boundaries.md）。
    let database =
        Database::from_pool(pool.clone(), DatabaseConfig::default()).map_err(|error| {
            BaseError::DatabaseInitFailed(format!("构造旧版唯一索引门禁查询失败：{error}"))
        })?;

    #[derive(sqlx::FromRow)]
    struct CountRow {
        count: i64,
    }
    let rows: Vec<CountRow> = database
        .query_with_params(
            "SELECT COUNT(*) AS count FROM information_schema.statistics \
             WHERE table_schema = DATABASE() \
               AND table_name = 'feishu_approval_task' \
               AND index_name = ?",
            vec![serde_json::json!(LEGACY_TASK_RECORD_INDEX)],
        )
        .await
        .map_err(|error| {
            BaseError::DatabaseInitFailed(format!("检查旧版审批任务唯一索引失败：{error}"))
        })?;
    if rows.first().map_or(0, |row| row.count) == 0 {
        return Ok(());
    }
    Err(BaseError::DatabaseInitFailed(format!(
        "检测到旧版全局唯一索引 {LEGACY_TASK_RECORD_INDEX}（feishu_approval_task.record_id \
         单列唯一）。它会导致复制多维表格后，同一 record_id 无法在另一配置下派发\
         （播种撞旧键被跳过、单条点击落库失败）。schema 同步只增不删，不会自动移除它。\
         请由 DBA 审核后手工执行（DB 写操作需单独批准）：\
         ALTER TABLE feishu_approval_task DROP INDEX {LEGACY_TASK_RECORD_INDEX};"
    )))
}

/// 在已经连接的测试或离线作业数据库上应用与应用启动完全相同的声明式 Schema。
pub async fn sync_with_database(
    database: Database,
    database_config: DatabaseConfig,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<SchemaSyncReport> {
    let initializer_database = Database::from_pool(database.pool().clone(), database_config)
        .map_err(anyhow::Error::from)?;
    let initializer = DatabaseInitializer::new(initializer_database);
    let guard_pool = database.pool().clone();
    let tools = Arc::new(ToolsBuilder::new().mysql(database).build()?);
    let result = async {
        ensure_legacy_task_record_index_absent(&guard_pool)
            .await
            .map_err(anyhow::Error::from)?;
        let application = crate::app::build_schema_app(Arc::clone(&tools), security)?;
        let definitions = definitions(&application.runtime)?;
        let references = definitions.iter().collect::<Vec<_>>();
        initializer
            .sync_table_definitions(&references)
            .await
            .map_err(anyhow::Error::from)
    }
    .await;
    tools.close().await;
    result
}

fn infrastructure_definitions() -> Result<[TableDefinition; 9], BaseError> {
    Ok([
        authorization_outbox()?,
        audit_event()?,
        // 权限组的三张运行支撑表：组-权限条目、用户-组关系、引导哨兵。
        crate::addon::access::domain::groups::tables::group_items_table_spec()?
            .table_definition()?,
        password_reset_token()?,
        crate::addon::access::domain::groups::tables::user_group_table_spec()?
            .table_definition()?,
        crate::addon::access::domain::groups::tables::system_owner_table_spec()?
            .table_definition()?,
        user_session()?,
        login_event()?,
        user_avatar()?,
    ])
}

pub(crate) fn authorization_outbox() -> Result<TableDefinition, BaseError> {
    Table::new("authorization_outbox")
        .fields([
            Field::id("id"),
            Field::bigint("user_id").required(),
            Field::bigint("authz_version").required(),
            Field::enumeration("state", ["pending", "processing", "published"])
                .required()
                .default("pending"),
            Field::integer("attempts").required().default(0),
            Field::bigint("available_at").required(),
            Field::bigint("lease_until"),
            Field::string("worker_id", 128),
            Field::bigint("created_at").required(),
            Field::bigint("published_at"),
            Field::string("last_error", 1024),
        ])
        .unique_named(
            "uk_authorization_outbox_user_version",
            ["user_id", "authz_version"],
        )
        .index_named(
            "idx_authorization_outbox_dispatch",
            ["state", "available_at", "id"],
        )
        .index_named(
            "idx_authorization_outbox_user_version",
            ["user_id", "authz_version"],
        )
        // 清理索引：delete_published_before 按 published_at 删除超过保留期的已发布行
        //（保留 7 天后分批清理，见 authorization-freshness-adr），无此索引每轮全表扫。
        .index_named("idx_authorization_outbox_published_at", ["published_at"])
        .build()
}

fn audit_event() -> Result<TableDefinition, BaseError> {
    Table::new("audit_event")
        .fields([
            Field::id("id"),
            Field::string("event_id", 32).required(),
            Field::integer("schema_version").required().default(1),
            Field::bigint("occurred_at").required(),
            Field::enumeration("actor_type", ["user", "system"]).required(),
            Field::string("actor_id", 128).required(),
            Field::bigint("tenant_id"),
            Field::string("action", 128).required(),
            Field::string("subject_type", 64),
            Field::string("subject_id", 128),
            Field::string("target_type", 64).required(),
            Field::string("target_id", 128).required(),
            Field::json("before_summary"),
            Field::json("after_summary"),
            Field::string("request_id", 32).required(),
            Field::enumeration("result", ["succeeded", "denied", "failed"]).required(),
        ])
        .unique_named("uk_audit_event_event_id", ["event_id"])
        .index_named(
            "idx_audit_event_actor",
            ["actor_type", "actor_id", "occurred_at", "id"],
        )
        .index_named(
            "idx_audit_event_subject",
            ["subject_type", "subject_id", "occurred_at", "id"],
        )
        .index_named(
            "idx_audit_event_target",
            ["target_type", "target_id", "occurred_at", "id"],
        )
        .index_named(
            "idx_audit_event_tenant",
            ["tenant_id", "occurred_at", "id"],
        )
        .index_named("idx_audit_event_request", ["request_id", "id"])
        .index_named("idx_audit_event_retention", ["occurred_at", "id"])
        .check_named(
            "chk_audit_event_event_id",
            "REGEXP_LIKE((`event_id` COLLATE utf8mb4_bin), '^[0-9a-f]{32}$')",
        )
        .check_named(
            "chk_audit_event_request_id",
            "REGEXP_LIKE((`request_id` COLLATE utf8mb4_bin), '^[0-9a-f]{32}$')",
        )
        .check_named(
            "chk_audit_event_subject_pair",
            "((`subject_type` IS NULL) AND (`subject_id` IS NULL)) OR ((`subject_type` IS NOT NULL) AND (`subject_id` IS NOT NULL))",
        )
        .check_named(
            "chk_audit_event_tenant_id",
            "(`tenant_id` IS NULL) OR (`tenant_id` > 0)",
        )
        .build()
}

fn password_reset_token() -> Result<TableDefinition, BaseError> {
    Table::new("password_reset_token")
        .fields([
            Field::id("id"),
            Field::string("token_digest", 64).required(),
            Field::string("token_fingerprint", 16).required(),
            Field::bigint("user_user").required(),
            // 自助找回场景没有请求者，写 NULL；管理签发（路线图阶段 D）再写入操作者。
            Field::bigint("requested_by_user"),
            Field::bigint("expires_at").required(),
            Field::bigint("consumed_at"),
            Field::bigint("invalidated_at"),
            Field::bigint("created_at").required(),
        ])
        .unique_named("uk_password_reset_token_digest", ["token_digest"])
        .index_named(
            "idx_password_reset_token_user_active",
            [
                "user_user",
                "consumed_at",
                "invalidated_at",
                "expires_at",
                "id",
            ],
        )
        .index_named("idx_password_reset_token_expiry", ["expires_at", "id"])
        .index_named(
            "idx_password_reset_token_requester",
            ["requested_by_user", "created_at", "id"],
        )
        .check_named(
            "chk_password_reset_token_expiry",
            "`expires_at` > `created_at`",
        )
        .check_named(
            "chk_password_reset_token_consumed",
            "(`consumed_at` IS NULL) OR (`consumed_at` >= `created_at`)",
        )
        .check_named(
            "chk_password_reset_token_invalidated",
            "(`invalidated_at` IS NULL) OR (`invalidated_at` >= `created_at`)",
        )
        .foreign_key_named(
            "fk_password_reset_token_user",
            ["user_user"],
            "users",
            ["id"],
        )
        .foreign_key_named(
            "fk_password_reset_token_requested_by",
            ["requested_by_user"],
            "users",
            ["id"],
        )
        .build()
}

/// 跨 Refresh 轮换稳定的会话记录（路线图 C-1）。
///
/// `session_id` 是 Token access claims 中的稳定标识，登录生成、refresh 继承；
/// `current_jti` 随每次轮换更新，供精确撤销（踢出设备）时黑名单定位。
/// 该表不属于任何 UI Catalog 业务表，与 authorization_outbox 等同列运行支撑。
pub(crate) fn user_session() -> Result<TableDefinition, BaseError> {
    Table::new("user_session")
        .fields([
            Field::string("session_id", 64).required().primary_key(),
            Field::bigint("user_id").required(),
            Field::string("current_jti", 64).required(),
            Field::string("refresh_jti", 64),
            Field::bigint("created_at").required(),
            Field::bigint("last_seen_at").required(),
            Field::string("ip", 64).required(),
            Field::string("user_agent", 512).required(),
            Field::bigint("revoked_at"),
        ])
        .unique_named("uk_user_session_id", ["session_id"])
        .index_named(
            "idx_user_session_user_active",
            ["user_id", "revoked_at", "last_seen_at", "session_id"],
        )
        .build()
}

/// 登录成功/失败的安全事件（路线图 C-2）。
///
/// 不落审计库（保留期清理会牵连用户可见历史），自带保留策略；
/// `failure_reason` 只记粗粒度原因，不记录明文凭据。
pub(crate) fn login_event() -> Result<TableDefinition, BaseError> {
    Table::new("login_event")
        .fields([
            Field::id("id"),
            Field::bigint("user_id").required(),
            Field::bigint("occurred_at").required(),
            Field::string("ip", 64).required(),
            Field::string("user_agent", 512).required(),
            Field::enumeration("result", ["succeeded", "failed"]).required(),
            Field::enumeration(
                "failure_reason",
                [
                    "invalid_password",
                    "user_not_found",
                    "disabled",
                    "rate_limited",
                ],
            ),
        ])
        .index_named(
            "idx_login_event_user_time",
            ["user_id", "occurred_at", "id"],
        )
        .build()
}

/// 用户头像（一人一行的数据库存储，不引入对象存储）。
///
/// 主键即用户 ID；图片字节以 base64 文本落 `content_base64`（TEXT 上限 65535
/// 字节，应用层限制解码前 ≤ 40 KiB），`etag` 是内容 sha256 十六进制前 32 字符，
/// 供前端按 `avatar_version` 做缓存失效。该表不进 UI Catalog，与 user_session
/// 同列运行支撑。
pub(crate) fn user_avatar() -> Result<TableDefinition, BaseError> {
    Table::new("user_avatar")
        .fields([
            Field::bigint("user_id").required().primary_key(),
            Field::string("mime", 32).required(),
            Field::text("content_base64").required(),
            Field::string("etag", 32).required(),
            Field::bigint("updated_at").required(),
        ])
        .check_named(
            "chk_user_avatar_mime",
            "`mime` IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')",
        )
        .foreign_key_named("fk_user_avatar_user", ["user_id"], "users", ["id"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infrastructure_schema_is_complete_and_versionless() {
        let definitions = infrastructure_definitions()
            .unwrap_or_else(|error| panic!("运行支撑表定义应有效: {error}"));
        assert_eq!(
            definitions.map(|definition| definition.name().to_string()),
            [
                "authorization_outbox",
                "audit_event",
                "permission_group_item",
                "password_reset_token",
                "user_group",
                "system_owner",
                "user_session",
                "login_event",
                "user_avatar",
            ]
        );
    }

    /// 幂等清理：只在索引存在时 DROP。测试可能在任意一步失败，清理本身不能报
    /// 「索引不存在」。
    async fn drop_legacy_index_if_present(pool: &sqlx::MySqlPool) -> anyhow::Result<()> {
        let present: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'feishu_approval_task' \
               AND index_name = ?",
        )
        .bind(LEGACY_TASK_RECORD_INDEX)
        .fetch_one(pool)
        .await?;
        if present > 0 {
            sqlx::query(
                "ALTER TABLE feishu_approval_task DROP INDEX uk_feishu_approval_task_record_id",
            )
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    /// 旧库残留 `uk_feishu_approval_task_record_id`（record_id 单列唯一）时，
    /// 跨配置同 record_id 会在播种/单条点击处失败，而 `schema_sync` 只增不删。
    /// 门禁必须 fail-closed：命中即拒绝，且文案给出可执行的 ALTER；清掉后放行。
    #[tokio::test]
    #[ignore = "需要独立 MySQL 测试库"]
    async fn real_mysql_legacy_task_record_index_guard_blocks_startup() -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let database = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(database.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "只允许测试库");
        let pool = database.pool().clone();
        // 先清理上次中断可能留下的旧索引——否则下面的 schema 同步会被本门禁挡住。
        drop_legacy_index_if_present(&pool).await?;

        sync_with_database(
            database,
            DatabaseConfig::default(),
            Arc::new(SecuritySettings::default()),
        )
        .await?;
        let database = yang_db::Database::connect(&url).await?;
        let pool = database.pool();

        // 全新库 / 已清理的库：放行。
        ensure_legacy_task_record_index_absent(pool).await?;

        // 测试库是多用例共享的：其它用例按新语义会留下「同 record_id 跨配置」的行，
        // 不先收敛就建不出旧全局唯一索引，测试会以 1062 假失败（不是门禁的问题）。
        sqlx::query(
            "DELETE t FROM feishu_approval_task t JOIN ( \
               SELECT record_id, MIN(id) AS keep_id FROM feishu_approval_task \
               GROUP BY record_id HAVING COUNT(*) > 1 \
             ) d ON t.record_id = d.record_id AND t.id <> d.keep_id",
        )
        .execute(pool)
        .await?;
        // 手工迁移遗漏的旧全局唯一索引：必须挡住启动。
        sqlx::query(
            "CREATE UNIQUE INDEX uk_feishu_approval_task_record_id \
             ON feishu_approval_task (record_id)",
        )
        .execute(pool)
        .await?;
        // 此时现行的 (config_id, record_id) 复合唯一键仍在（部分手工迁移的形态）：
        // 两个索引并存也必须报错——旧的还在就不算迁移完成。
        let composite: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'feishu_approval_task' \
               AND index_name = 'uk_feishu_approval_task_config_record'",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            composite > 0,
            "现行复合唯一键应仍在，形成两索引并存的回归形态"
        );

        let error = ensure_legacy_task_record_index_absent(pool)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("旧版全局唯一索引残留必须挡住启动"))?;
        let message = error.to_string();
        assert!(
            message.contains("DROP INDEX") && message.contains("uk_feishu_approval_task_record_id"),
            "错误文案必须给出可执行的 ALTER: {message}"
        );

        // 清理必须幂等，清掉后门禁放行。
        drop_legacy_index_if_present(pool).await?;
        drop_legacy_index_if_present(pool).await?;
        ensure_legacy_task_record_index_absent(pool).await?;
        Ok(())
    }
}
