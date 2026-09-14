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

/// 在已经连接的测试或离线作业数据库上应用与应用启动完全相同的声明式 Schema。
pub async fn sync_with_database(
    database: Database,
    database_config: DatabaseConfig,
    security: Arc<SecuritySettings>,
) -> anyhow::Result<SchemaSyncReport> {
    let initializer_database = Database::from_pool(database.pool().clone(), database_config)
        .map_err(anyhow::Error::from)?;
    let initializer = DatabaseInitializer::new(initializer_database);
    let tools = Arc::new(ToolsBuilder::new().mysql(database).build()?);
    let result = async {
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

fn infrastructure_definitions() -> Result<[TableDefinition; 6], BaseError> {
    Ok([
        authorization_outbox()?,
        audit_event()?,
        password_reset_token()?,
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
                "password_reset_token",
                "user_session",
                "login_event",
                "user_avatar",
            ]
        );
    }
}
