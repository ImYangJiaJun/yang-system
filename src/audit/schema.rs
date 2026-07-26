use super::AUDIT_EVENT_TABLE;
use anyhow::{ensure, Context};
use sqlx::MySqlPool;
use std::collections::{BTreeMap, BTreeSet};

type ColumnShape = (String, String, String, Option<String>, String);

pub(crate) async fn validate_schema(pool: &MySqlPool) -> anyhow::Result<()> {
    let table_shape: Option<(String, String)> = sqlx::query_as(
        "SELECT ENGINE, TABLE_COLLATION FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = ?",
    )
    .bind(AUDIT_EVENT_TABLE)
    .fetch_optional(pool)
    .await
    .context("读取审计表属性失败")?;
    ensure!(
        table_shape == Some(("InnoDB".to_string(), "utf8mb4_unicode_ci".to_string())),
        "audit_event 必须使用 InnoDB 与 utf8mb4_unicode_ci，请先执行版本化迁移: {table_shape:?}"
    );

    let columns: Vec<ColumnShape> = sqlx::query_as(
        "SELECT CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR), \
                CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_DEFAULT AS CHAR), CAST(EXTRA AS CHAR) \
         FROM information_schema.columns \
         WHERE table_schema = DATABASE() AND table_name = ? \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(AUDIT_EVENT_TABLE)
    .fetch_all(pool)
    .await
    .context("读取审计表列失败")?;
    let expected_columns: Vec<ColumnShape> = vec![
        shape("id", "bigint", "NO", None, "auto_increment"),
        shape("event_id", "char(32)", "NO", None, ""),
        shape("schema_version", "smallint unsigned", "NO", Some("1"), ""),
        shape("occurred_at", "bigint", "NO", None, ""),
        shape("actor_type", "enum('user','system')", "NO", None, ""),
        shape("actor_id", "varchar(128)", "NO", None, ""),
        shape("tenant_id", "bigint", "YES", None, ""),
        shape("action", "varchar(128)", "NO", None, ""),
        shape("subject_type", "varchar(64)", "YES", None, ""),
        shape("subject_id", "varchar(128)", "YES", None, ""),
        shape("target_type", "varchar(64)", "NO", None, ""),
        shape("target_id", "varchar(128)", "NO", None, ""),
        shape("before_summary", "json", "YES", None, ""),
        shape("after_summary", "json", "YES", None, ""),
        shape("request_id", "char(32)", "NO", None, ""),
        shape(
            "result",
            "enum('succeeded','denied','failed')",
            "NO",
            None,
            "",
        ),
    ];
    ensure!(
        columns == expected_columns,
        "audit_event 列未对齐，请先执行版本化迁移: {columns:?}"
    );

    let index_rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT INDEX_NAME, NON_UNIQUE, \
                GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR ',') \
         FROM information_schema.statistics \
         WHERE table_schema = DATABASE() AND table_name = ? \
         GROUP BY INDEX_NAME, NON_UNIQUE",
    )
    .bind(AUDIT_EVENT_TABLE)
    .fetch_all(pool)
    .await
    .context("读取审计表索引失败")?;
    let indexes = index_rows
        .into_iter()
        .map(|(name, non_unique, columns)| (name, (non_unique, columns)))
        .collect::<BTreeMap<_, _>>();
    for (name, non_unique, columns) in expected_indexes() {
        ensure!(
            indexes.get(name) == Some(&(non_unique, columns.to_string())),
            "audit_event 索引 {name} 未对齐，请先执行版本化迁移: {indexes:?}"
        );
    }

    let constraints = sqlx::query_scalar::<_, String>(
        "SELECT CONSTRAINT_NAME FROM information_schema.table_constraints \
         WHERE table_schema = DATABASE() AND table_name = ? AND CONSTRAINT_TYPE = 'CHECK'",
    )
    .bind(AUDIT_EVENT_TABLE)
    .fetch_all(pool)
    .await
    .context("读取审计表 CHECK 约束失败")?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let expected_constraints = [
        "chk_audit_event_event_id",
        "chk_audit_event_request_id",
        "chk_audit_event_subject_pair",
        "chk_audit_event_tenant_id",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    ensure!(
        constraints == expected_constraints,
        "audit_event CHECK 约束未对齐，请先执行版本化迁移: {constraints:?}"
    );
    Ok(())
}

fn shape(
    name: &str,
    column_type: &str,
    nullable: &str,
    default: Option<&str>,
    extra: &str,
) -> ColumnShape {
    (
        name.to_string(),
        column_type.to_string(),
        nullable.to_string(),
        default.map(str::to_string),
        extra.to_string(),
    )
}

fn expected_indexes() -> [(&'static str, i64, &'static str); 8] {
    [
        ("PRIMARY", 0, "id"),
        ("uk_audit_event_event_id", 0, "event_id"),
        (
            "idx_audit_event_actor",
            1,
            "actor_type,actor_id,occurred_at,id",
        ),
        (
            "idx_audit_event_subject",
            1,
            "subject_type,subject_id,occurred_at,id",
        ),
        (
            "idx_audit_event_target",
            1,
            "target_type,target_id,occurred_at,id",
        ),
        ("idx_audit_event_tenant", 1, "tenant_id,occurred_at,id"),
        ("idx_audit_event_request", 1, "request_id,id"),
        ("idx_audit_event_retention", 1, "occurred_at,id"),
    ]
}
