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
    let initializer = DatabaseInitializer::new(initializer_database, false);
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

fn infrastructure_definitions() -> Result<[TableDefinition; 3], BaseError> {
    Ok([
        authorization_outbox()?,
        audit_event()?,
        password_reset_token()?,
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
            Field::bigint("requested_by_user").required(),
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
            ]
        );
    }
}
