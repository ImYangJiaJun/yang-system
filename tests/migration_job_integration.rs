use anyhow::{ensure, Context};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;
use yang_base::database::MigrationPlanStatus;
use yang_db::{Database, DatabaseConfig};
use yang_system::config::SecuritySettings;
use yang_system::migrations::{execute_with_database, MigrationCommand, MigrationRunReport};

const BUSINESS_TABLES: [&str; 6] = [
    "work_task",
    "work_project",
    "org_user",
    "org_org",
    "admin_user",
    "users",
];
const INTERNAL_TABLES: [&str; 3] = [
    "password_reset_token",
    "audit_event",
    "authorization_outbox",
];

fn database_config() -> DatabaseConfig {
    DatabaseConfig::default()
        .with_max_connections(4)
        .with_min_connections(0)
        .with_connect_timeout(10)
}

fn security_settings() -> Arc<SecuritySettings> {
    Arc::new(SecuritySettings {
        argon2_max_concurrency: 2,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 1_000,
        auth_rate_limit_username_attempts: 100,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: false,
        trusted_proxy_cidrs: Vec::new(),
    })
}

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect_with_config(&url, database_config())
        .await
        .context("连接迁移测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取迁移测试数据库名失败")?;
    let name = name.context("迁移测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行版本化迁移测试"
    );
    Ok(database)
}

async fn run_job(command: MigrationCommand) -> anyhow::Result<MigrationRunReport> {
    execute_with_database(
        command,
        connect_test_database().await?,
        database_config(),
        security_settings(),
    )
    .await
}

async fn reset_test_database(database: &Database) -> anyhow::Result<()> {
    for table in INTERNAL_TABLES.into_iter().chain(BUSINESS_TABLES) {
        let statement = format!("DROP TABLE IF EXISTS `{table}`");
        sqlx::query(&statement)
            .execute(database.pool())
            .await
            .with_context(|| format!("清理迁移测试表失败: {table}"))?;
    }
    sqlx::query("DROP TABLE IF EXISTS `_migrations`")
        .execute(database.pool())
        .await
        .context("清理迁移记录表失败")?;
    Ok(())
}

async fn users_status_check(database: &Database) -> anyhow::Result<Option<(String, String)>> {
    check_constraint_metadata(database, "users", "chk_users_status").await
}

#[cfg(any())]
async fn bootstrap_key_check(database: &Database) -> anyhow::Result<Option<(String, String)>> {
    check_constraint_metadata(database, "admin_user", "chk_admin_user_bootstrap_key").await
}

async fn check_constraint_metadata(
    database: &Database,
    table: &str,
    constraint: &str,
) -> anyhow::Result<Option<(String, String)>> {
    sqlx::query_as(
        "SELECT CAST(tc.ENFORCED AS CHAR), CAST(cc.CHECK_CLAUSE AS CHAR) \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.check_constraints cc \
           ON cc.constraint_schema = tc.constraint_schema \
          AND cc.constraint_name = tc.constraint_name \
         WHERE tc.table_schema = DATABASE() \
           AND tc.table_name = ? \
           AND tc.constraint_type = 'CHECK' \
           AND tc.constraint_name = ?",
    )
    .bind(table)
    .bind(constraint)
    .fetch_optional(database.pool())
    .await
    .with_context(|| format!("读取 {table}.{constraint} CHECK 元数据失败"))
}

async fn foreign_key_metadata(
    database: &Database,
    constraint: &str,
) -> anyhow::Result<Vec<(String, String, String, String, String, String)>> {
    sqlx::query_as(
        "SELECT CAST(rc.TABLE_NAME AS CHAR), CAST(kcu.COLUMN_NAME AS CHAR), \
                CAST(kcu.REFERENCED_TABLE_NAME AS CHAR), \
                CAST(kcu.REFERENCED_COLUMN_NAME AS CHAR), \
                CAST(rc.UPDATE_RULE AS CHAR), CAST(rc.DELETE_RULE AS CHAR) \
         FROM information_schema.referential_constraints AS rc \
         INNER JOIN information_schema.key_column_usage AS kcu \
           ON kcu.CONSTRAINT_SCHEMA = rc.CONSTRAINT_SCHEMA \
          AND kcu.TABLE_NAME = rc.TABLE_NAME \
          AND kcu.CONSTRAINT_NAME = rc.CONSTRAINT_NAME \
         WHERE rc.CONSTRAINT_SCHEMA = DATABASE() AND rc.CONSTRAINT_NAME = ? \
         ORDER BY kcu.ORDINAL_POSITION",
    )
    .bind(constraint)
    .fetch_all(database.pool())
    .await
    .with_context(|| format!("读取外键 {constraint} 元数据失败"))
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("迁移测试失败后清理也失败: {cleanup_error:#}")))
        }
    }
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 指向 _test MySQL 数据库"]
async fn versioned_job_is_read_only_in_plan_and_safe_across_apply_retry_and_drift(
) -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        let dry_run = run_job(MigrationCommand::Plan).await?;
        ensure!(
            dry_run
                .plan
                .entries
                .iter()
                .all(|entry| entry.status == MigrationPlanStatus::Pending),
            "空数据库的 dry-run 应全部为 pending"
        );
        ensure!(
            !control
                .table_exists(yang_db::table!("_migrations"))
                .await
                .context("检查 dry-run 是否写库失败")?,
            "dry-run 不得创建迁移记录表"
        );

        let applied = run_job(MigrationCommand::Apply).await?;
        ensure!(
            applied
                .plan
                .entries
                .iter()
                .all(|entry| entry.status == MigrationPlanStatus::Applied),
            "首次 apply 后全部版本必须为 applied"
        );
        let actual_tables = applied
            .validated_tables
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected_tables = BUSINESS_TABLES.into_iter().collect::<BTreeSet<_>>();
        ensure!(
            actual_tables == expected_tables,
            "apply 必须在返回前用当前应用定义校验全部业务表: {actual_tables:?}"
        );

        let migration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM `_migrations` WHERE module_name = 'yang-system' AND status = 'applied'",
        )
        .fetch_one(control.pool())
        .await
        .context("统计迁移执行记录失败")?;
        ensure!(migration_count == 17, "应记录 17 个 applied 版本");
        let server_identity: (String, String) =
            sqlx::query_as("SELECT CAST(VERSION() AS CHAR), CAST(@@version_comment AS CHAR)")
                .fetch_one(control.pool())
                .await
                .context("读取 MySQL 版本与发行标识失败")?;
        ensure!(
            !server_identity.0.trim().is_empty() && !server_identity.1.trim().is_empty(),
            "发布前必须能识别 MySQL 版本与发行实现"
        );
        let status_counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, COUNT(*) FROM users GROUP BY status ORDER BY status",
        )
        .fetch_all(control.pool())
        .await
        .context("执行用户状态分布预检失败")?;
        ensure!(
            status_counts
                .iter()
                .all(|(status, _)| status == "active" || status == "disabled"),
            "用户状态预检发现领域集合之外的值: {status_counts:?}"
        );
        let status_check = users_status_check(&control).await?;
        ensure!(
            matches!(&status_check, Some((enforced, clause)) if enforced == "YES" && clause.contains("active") && clause.contains("disabled")),
            "用户状态 CHECK 必须存在并强制执行: {status_check:?}"
        );
        for (constraint, expected) in [
            (
                "fk_admin_user_user_user",
                (
                    "admin_user",
                    "user_user",
                    "users",
                    "id",
                    "RESTRICT",
                    "RESTRICT",
                ),
            ),
            (
                "fk_org_user_user_user",
                (
                    "org_user",
                    "user_user",
                    "users",
                    "id",
                    "RESTRICT",
                    "RESTRICT",
                ),
            ),
            (
                "fk_org_user_org_org",
                (
                    "org_user",
                    "org_org",
                    "org_org",
                    "id",
                    "RESTRICT",
                    "RESTRICT",
                ),
            ),
        ] {
            let rows = foreign_key_metadata(&control, constraint).await?;
            ensure!(
                rows.len() == 1
                    && rows[0]
                        == (
                            expected.0.to_string(),
                            expected.1.to_string(),
                            expected.2.to_string(),
                            expected.3.to_string(),
                            expected.4.to_string(),
                            expected.5.to_string(),
                        ),
                "外键 {constraint} 必须精确匹配单列目标与双 RESTRICT: {rows:?}"
            );
        }
        let invalid_status = sqlx::query(
            "INSERT INTO users (username, password_hash, status, created_at, updated_at) \
             VALUES ('invalid_status_sentinel', 'hash', 'pending', 1, 1)",
        )
        .execute(control.pool())
        .await;
        ensure!(invalid_status.is_err(), "数据库必须拒绝领域集合之外的用户状态");
        let authz_version_shape: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT CAST(COLUMN_TYPE AS CHAR), CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_DEFAULT AS CHAR) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = 'users' AND column_name = 'authz_version'",
        )
        .fetch_optional(control.pool())
        .await
        .context("读取授权版本列结构失败")?;
        ensure!(
            authz_version_shape
                == Some(("bigint".to_string(), "NO".to_string(), Some("1".to_string()))),
            "authz_version 必须是 BIGINT NOT NULL DEFAULT 1: {authz_version_shape:?}"
        );
        let credential_version_shape: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT CAST(COLUMN_TYPE AS CHAR), CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_DEFAULT AS CHAR) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = 'users' AND column_name = 'credential_version'",
        )
        .fetch_optional(control.pool())
        .await
        .context("读取凭据版本列结构失败")?;
        ensure!(
            credential_version_shape
                == Some(("bigint".to_string(), "NO".to_string(), Some("0".to_string()))),
            "credential_version 必须是 BIGINT NOT NULL DEFAULT 0: {credential_version_shape:?}"
        );
        let outbox_indexes: BTreeSet<String> = sqlx::query_scalar(
            "SELECT DISTINCT INDEX_NAME FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = 'authorization_outbox'",
        )
        .fetch_all(control.pool())
        .await
        .context("读取授权 Outbox 索引失败")?
        .into_iter()
        .collect();
        ensure!(
            [
                "PRIMARY",
                "uk_authorization_outbox_user_version",
                "idx_authorization_outbox_dispatch",
                "idx_authorization_outbox_user_version",
            ]
            .into_iter()
            .all(|index| outbox_indexes.contains(index)),
            "授权 Outbox 必须具备主键、幂等键、派发索引与用户版本索引: {outbox_indexes:?}"
        );
        let audit_indexes: BTreeSet<String> = sqlx::query_scalar(
            "SELECT DISTINCT INDEX_NAME FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = 'audit_event'",
        )
        .fetch_all(control.pool())
        .await
        .context("读取审计表索引失败")?
        .into_iter()
        .collect();
        ensure!(
            [
                "PRIMARY",
                "uk_audit_event_event_id",
                "idx_audit_event_actor",
                "idx_audit_event_subject",
                "idx_audit_event_target",
                "idx_audit_event_tenant",
                "idx_audit_event_request",
                "idx_audit_event_retention",
            ]
            .into_iter()
            .all(|index| audit_indexes.contains(index)),
            "审计表必须具备幂等、检索和保留游标索引: {audit_indexes:?}"
        );
        let reset_indexes: BTreeSet<String> = sqlx::query_scalar(
            "SELECT DISTINCT INDEX_NAME FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = 'password_reset_token'",
        )
        .fetch_all(control.pool())
        .await
        .context("读取密码重置凭证索引失败")?
        .into_iter()
        .collect();
        ensure!(
            [
                "PRIMARY",
                "uk_password_reset_token_digest",
                "idx_password_reset_token_user_active",
                "idx_password_reset_token_expiry",
                "idx_password_reset_token_requester",
            ]
            .into_iter()
            .all(|index| reset_indexes.contains(index)),
            "密码重置表必须具备唯一摘要、活动凭证、到期清理与发起者索引: {reset_indexes:?}"
        );
        let invalid_request_id = sqlx::query(
            "INSERT INTO `audit_event` (`event_id`, `schema_version`, `occurred_at`, \
             `actor_type`, `actor_id`, `action`, `target_type`, `target_id`, \
             `after_summary`, `request_id`, `result`) \
             VALUES ('0123456789abcdef0123456789abcdef', 1, UNIX_TIMESTAMP(), \
             'user', '7', 'admin.user.bootstrap', 'admin_account', '1', \
             JSON_OBJECT('status', 'active'), 'not-a-request-id', 'succeeded')",
        )
        .execute(control.pool())
        .await;
        ensure!(
            invalid_request_id.is_err(),
            "数据库 CHECK 必须拒绝不可关联的 request_id"
        );
        let uppercase_event_id = sqlx::query(
            "INSERT INTO `audit_event` (`event_id`, `schema_version`, `occurred_at`, \
             `actor_type`, `actor_id`, `action`, `target_type`, `target_id`, \
             `after_summary`, `request_id`, `result`) \
             VALUES ('ABCDEF0123456789ABCDEF0123456789', 1, UNIX_TIMESTAMP(), \
             'user', '7', 'admin.user.bootstrap', 'admin_account', '1', \
             JSON_OBJECT('status', 'active'), '0123456789abcdef0123456789abcdef', 'succeeded')",
        )
        .execute(control.pool())
        .await;
        ensure!(
            uppercase_event_id.is_err(),
            "数据库 CHECK 必须拒绝非规范化大写 event_id"
        );
        sqlx::query(
            "INSERT INTO `users` (`username`, `password_hash`, `status`, `created_at`, `updated_at`) VALUES ('migration_sentinel', 'hash', 'active', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("写入幂等重跑哨兵失败")?;

        let sentinel_authz_version: i64 = sqlx::query_scalar(
            "SELECT `authz_version` FROM `users` WHERE username = 'migration_sentinel'",
        )
        .fetch_one(control.pool())
        .await
        .context("读取迁移哨兵授权版本失败")?;
        ensure!(sentinel_authz_version == 1, "新增用户必须取得授权版本默认值 1");
        let sentinel_credential_version: i64 = sqlx::query_scalar(
            "SELECT `credential_version` FROM `users` WHERE username = 'migration_sentinel'",
        )
        .fetch_one(control.pool())
        .await
        .context("读取迁移哨兵凭据版本失败")?;
        ensure!(
            sentinel_credential_version == 0,
            "新增用户必须取得凭据版本默认值 0"
        );

        sqlx::query(
            "UPDATE `_migrations` SET status = 'running' WHERE module_name = 'yang-system' AND version IN ('20260726_0004_create_org_user', '20260726_0005_add_user_authz_version', '20260726_0006_create_authorization_outbox', '20260726_0007_create_audit_event', '20260731_0008_create_work_project', '20260731_0009_create_work_task', '20260731_0010_add_user_credential_version', '20260731_0011_create_password_reset_token', '20260731_0012_add_users_status_check', '20260731_0013_add_admin_user_user_fk', '20260731_0014_add_org_user_user_fk', '20260731_0015_add_org_user_org_fk', '20260731_0016_add_admin_bootstrap_key_check')",
        )
        .execute(control.pool())
        .await
        .context("模拟幂等建表和原子 ALTER 两类迁移作业中断失败")?;
        let retried = run_job(MigrationCommand::Apply).await?;
        ensure!(
            retried
                .plan
                .entries
                .iter()
                .all(|entry| entry.status == MigrationPlanStatus::Applied),
            "中断重跑后全部版本必须恢复为 applied"
        );
        let sentinel_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM `users` WHERE username = 'migration_sentinel'")
                .fetch_one(control.pool())
                .await
                .context("检查幂等重跑哨兵失败")?;
        ensure!(sentinel_count == 1, "幂等重跑不得破坏或重复业务数据");

        sqlx::query("ALTER TABLE `audit_event` DROP INDEX `idx_audit_event_request`")
            .execute(control.pool())
            .await
            .context("构造审计表索引漂移失败")?;
        let audit_drift_error = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("审计表结构漂移必须阻止迁移后 validate"),
            Err(error) => error,
        };
        ensure!(
            format!("{audit_drift_error:#}").contains("idx_audit_event_request"),
            "审计结构漂移错误必须定位具体索引: {audit_drift_error:#}"
        );
        sqlx::query(
            "ALTER TABLE `audit_event` ADD KEY `idx_audit_event_request` (`request_id`, `id`)",
        )
        .execute(control.pool())
        .await
        .context("恢复审计表请求索引失败")?;

        sqlx::query(
            "UPDATE `_migrations` SET checksum = '0000000000000000' WHERE module_name = 'yang-system' AND version = '20260726_0001_create_users'",
        )
        .execute(control.pool())
        .await
        .context("构造 checksum 漂移失败")?;
        let drift = run_job(MigrationCommand::Plan).await?;
        ensure!(
            drift.plan.entries[0].status == MigrationPlanStatus::ChecksumMismatch,
            "dry-run 必须显式报告 checksum 漂移"
        );
        let error = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("checksum 漂移必须阻止 apply"),
            Err(error) => error,
        };
        ensure!(
            format!("{error:#}").contains("20260726_0001_create_users"),
            "漂移错误必须定位具体版本: {error:#}"
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 指向 _test MySQL 数据库"]
async fn status_check_upgrade_rejects_dirty_data_and_same_name_drift() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        run_job(MigrationCommand::Apply).await?;
        sqlx::query("ALTER TABLE users DROP CHECK chk_users_status")
            .execute(control.pool())
            .await
            .context("构造 0011 到 0012 的既有库升级起点失败")?;
        sqlx::query(
            "DELETE FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version = '20260731_0012_add_users_status_check'",
        )
        .execute(control.pool())
        .await
        .context("移除 0012 迁移记录失败")?;
        sqlx::query(
            "INSERT INTO users (username, password_hash, status, created_at, updated_at) \
             VALUES ('dirty_status_sentinel', 'hash', 'pending', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("构造用户状态脏数据失败")?;

        let dirty_error = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("脏状态数据必须阻止 CHECK 迁移"),
            Err(error) => error,
        };
        ensure!(
            format!("{dirty_error:#}").contains("用户状态 CHECK 发布预检发现 1 行"),
            "脏数据失败必须在 DDL 前给出计数诊断: {dirty_error:#}"
        );
        let migration_status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version = '20260731_0012_add_users_status_check'",
        )
        .fetch_optional(control.pool())
        .await
        .context("读取脏数据失败后的迁移状态失败")?;
        ensure!(
            migration_status.as_deref() != Some("applied"),
            "DDL 失败不得把 0012 标记为 applied: {migration_status:?}"
        );
        ensure!(
            users_status_check(&control).await?.is_none(),
            "脏数据失败后不得遗留半完成 CHECK"
        );

        sqlx::query(
            "UPDATE users SET status = 'disabled' WHERE username = 'dirty_status_sentinel'",
        )
        .execute(control.pool())
        .await
        .context("修复用户状态脏数据失败")?;
        let upgraded = run_job(MigrationCommand::Apply).await?;
        ensure!(
            upgraded
                .plan
                .entries
                .iter()
                .all(|entry| entry.status == MigrationPlanStatus::Applied),
            "修复脏数据后的既有库升级必须完成"
        );

        sqlx::query("ALTER TABLE users DROP CHECK chk_users_status")
            .execute(control.pool())
            .await
            .context("移除正确状态 CHECK 失败")?;
        sqlx::query("ALTER TABLE users ADD CONSTRAINT chk_users_status CHECK (status <> '')")
            .execute(control.pool())
            .await
            .context("构造同名异义 CHECK 失败")?;
        sqlx::query(
            "UPDATE `_migrations` SET status = 'running' \
             WHERE module_name = 'yang-system' \
             AND version = '20260731_0012_add_users_status_check'",
        )
        .execute(control.pool())
        .await
        .context("构造 0012 DDL 中断状态失败")?;
        let mismatch = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("同名异义 CHECK 必须阻止中断恢复"),
            Err(error) => error,
        };
        let mismatch_text = format!("{mismatch:#}");
        ensure!(
            mismatch_text.contains("chk_users_status")
                && (mismatch_text.contains("表达式")
                    || mismatch_text.contains("Duplicate check constraint name")),
            "完成探针必须拒绝并定位同名异义 CHECK: {mismatch_text}"
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 指向 _test MySQL 数据库"]
#[cfg(any())]
async fn bootstrap_key_check_rejects_dirty_values_and_same_name_drift() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        run_job(MigrationCommand::Apply).await?;
        sqlx::query("ALTER TABLE admin_user DROP CHECK chk_admin_user_bootstrap_key")
            .execute(control.pool())
            .await
            .context("构造 0015 到 0016 的既有库升级起点失败")?;
        sqlx::query(
            "DELETE FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version = '20260731_0016_add_admin_bootstrap_key_check'",
        )
        .execute(control.pool())
        .await
        .context("移除 0016 迁移记录失败")?;

        let first_user = sqlx::query(
            "INSERT INTO users (username, password_hash, status, created_at, updated_at) \
             VALUES ('bootstrap_check_first', 'hash', 'active', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("构造 bootstrap_key CHECK 用户失败")?
        .last_insert_id();
        sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, status, admin, bootstrap_key, created_at, updated_at) \
             VALUES (?, 'Bootstrap Check', 'active', TRUE, 'unexpected-bootstrap', 1, 1)",
        )
        .bind(first_user)
        .execute(control.pool())
        .await
        .context("构造 bootstrap_key 脏数据失败")?;

        let dirty_error = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("非法 bootstrap_key 必须在 DDL 前阻止升级"),
            Err(error) => error,
        };
        ensure!(
            format!("{dirty_error:#}").contains("bootstrap_key CHECK 发布预检发现 1 行"),
            "脏数据失败必须给出计数诊断: {dirty_error:#}"
        );
        ensure!(
            bootstrap_key_check(&control).await?.is_none(),
            "脏数据失败后不得遗留半完成 CHECK"
        );

        sqlx::query("UPDATE admin_user SET bootstrap_key = NULL WHERE user_user = ?")
            .bind(first_user)
            .execute(control.pool())
            .await
            .context("修复 bootstrap_key 脏数据失败")?;
        run_job(MigrationCommand::Apply).await?;
        let (enforced, expression) = bootstrap_key_check(&control)
            .await?
            .context("0016 完成后必须存在精确 CHECK")?;
        ensure!(
            enforced.eq_ignore_ascii_case("YES")
                && expression.contains("bootstrap_key")
                && expression.contains("initial-admin"),
            "bootstrap_key CHECK 元数据必须为 ENFORCED 且绑定保留值: {enforced} {expression}"
        );

        let invalid_update = sqlx::query(
            "UPDATE admin_user SET bootstrap_key = 'unexpected-bootstrap' WHERE user_user = ?",
        )
        .bind(first_user)
        .execute(control.pool())
        .await;
        ensure!(
            invalid_update.is_err(),
            "CHECK 必须拒绝任意其他非空 bootstrap_key"
        );
        sqlx::query("UPDATE admin_user SET bootstrap_key = 'initial-admin' WHERE user_user = ?")
            .bind(first_user)
            .execute(control.pool())
            .await
            .context("CHECK 必须允许唯一 initial-admin 哨兵")?;

        let second_user = sqlx::query(
            "INSERT INTO users (username, password_hash, status, created_at, updated_at) \
             VALUES ('bootstrap_check_second', 'hash', 'active', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("构造第二个 bootstrap_key CHECK 用户失败")?
        .last_insert_id();
        sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, status, admin, bootstrap_key, created_at, updated_at) \
             VALUES (?, 'Ordinary Admin', 'active', TRUE, NULL, 1, 1)",
        )
        .bind(second_user)
        .execute(control.pool())
        .await
        .context("NULL bootstrap_key 必须允许普通平台授权关系共存")?;
        let duplicate_sentinel = sqlx::query(
            "UPDATE admin_user SET bootstrap_key = 'initial-admin' WHERE user_user = ?",
        )
        .bind(second_user)
        .execute(control.pool())
        .await;
        ensure!(
            duplicate_sentinel.is_err(),
            "NULL UNIQUE 与 CHECK 组合必须拒绝第二个 initial-admin 哨兵"
        );

        sqlx::query("ALTER TABLE admin_user DROP CHECK chk_admin_user_bootstrap_key")
            .execute(control.pool())
            .await
            .context("移除正确 bootstrap_key CHECK 失败")?;
        sqlx::query(
            "ALTER TABLE admin_user ADD CONSTRAINT chk_admin_user_bootstrap_key \
             CHECK (bootstrap_key IS NULL OR bootstrap_key <> '')",
        )
        .execute(control.pool())
        .await
        .context("构造同名异义 bootstrap_key CHECK 失败")?;
        sqlx::query(
            "UPDATE `_migrations` SET status = 'running' \
             WHERE module_name = 'yang-system' \
               AND version = '20260731_0016_add_admin_bootstrap_key_check'",
        )
        .execute(control.pool())
        .await
        .context("构造 0016 DDL 中断状态失败")?;
        let mismatch = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("同名异义 bootstrap_key CHECK 必须阻止中断恢复"),
            Err(error) => error,
        };
        let mismatch_text = format!("{mismatch:#}");
        ensure!(
            mismatch_text.contains("chk_admin_user_bootstrap_key")
                && (mismatch_text.contains("表达式")
                    || mismatch_text.contains("Duplicate check constraint name")),
            "完成探针必须拒绝并定位同名异义 CHECK: {mismatch_text}"
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 指向 _test MySQL 数据库"]
async fn authorization_foreign_key_upgrade_rejects_orphans_and_recovers_completed_ddl(
) -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        run_job(MigrationCommand::Apply).await?;
        for (table, constraint) in [
            ("admin_user", "fk_admin_user_user_user"),
            ("org_user", "fk_org_user_user_user"),
            ("org_user", "fk_org_user_org_org"),
        ] {
            let statement = format!("ALTER TABLE `{table}` DROP FOREIGN KEY `{constraint}`");
            sqlx::query(&statement)
                .execute(control.pool())
                .await
                .with_context(|| format!("移除外键 {constraint} 以准备升级测试失败"))?;
        }
        sqlx::query(
            "DELETE FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version IN ('20260731_0013_add_admin_user_user_fk', \
                             '20260731_0014_add_org_user_user_fk', \
                             '20260731_0015_add_org_user_org_fk')",
        )
        .execute(control.pool())
        .await
        .context("准备授权外键升级起点失败")?;

        sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, admin, status, created_at, updated_at) \
             VALUES (999999, 'orphan_admin', 0, 'active', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("构造孤儿平台授权事实失败")?;
        let orphan_error = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("孤儿授权事实必须阻止外键 DDL"),
            Err(error) => error,
        };
        let orphan_text = format!("{orphan_error:#}");
        ensure!(
            orphan_text.contains("fk_admin_user_user_user")
                && orphan_text.contains("1 行孤儿授权事实"),
            "外键发布预检必须定位约束并报告孤儿计数: {orphan_text}"
        );
        let first_fk_status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version = '20260731_0013_add_admin_user_user_fk'",
        )
        .fetch_optional(control.pool())
        .await
        .context("读取孤儿预检失败后的迁移状态失败")?;
        ensure!(
            first_fk_status.as_deref() != Some("applied"),
            "孤儿预检失败不得把 0013 标记为 applied: {first_fk_status:?}"
        );
        ensure!(
            foreign_key_metadata(&control, "fk_admin_user_user_user")
                .await?
                .is_empty(),
            "孤儿预检失败不得遗留半完成外键"
        );

        sqlx::query("DELETE FROM admin_user WHERE name = 'orphan_admin'")
            .execute(control.pool())
            .await
            .context("修复孤儿平台授权事实失败")?;
        let upgraded = run_job(MigrationCommand::Apply).await?;
        ensure!(
            upgraded
                .plan
                .entries
                .iter()
                .all(|entry| entry.status == MigrationPlanStatus::Applied),
            "修复孤儿数据后授权外键升级必须完成"
        );

        sqlx::query(
            "INSERT INTO users (username, password_hash, status, created_at, updated_at) \
             VALUES ('fk_parent_user', 'hash', 'active', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("写入外键父用户失败")?;
        let user_id: i64 =
            sqlx::query_scalar("SELECT id FROM users WHERE username = 'fk_parent_user'")
                .fetch_one(control.pool())
                .await
                .context("读取外键父用户失败")?;
        sqlx::query(
            "INSERT INTO org_org (code, name, status, created_at) \
             VALUES ('fk_parent_org', 'FK Parent Org', 'active', 1)",
        )
        .execute(control.pool())
        .await
        .context("写入外键父企业失败")?;
        let org_id: i64 = sqlx::query_scalar("SELECT id FROM org_org WHERE code = 'fk_parent_org'")
            .fetch_one(control.pool())
            .await
            .context("读取外键父企业失败")?;
        sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, admin, status, created_at, updated_at) \
             VALUES (?, 'fk_admin', 0, 'active', 1, 1)",
        )
        .bind(user_id)
        .execute(control.pool())
        .await
        .context("写入受保护平台授权事实失败")?;
        sqlx::query(
            "INSERT INTO org_user \
             (org_org, user_user, name, admin, status, created_at, updated_at) \
             VALUES (?, ?, 'fk_member', 0, 'active', 1, 1)",
        )
        .bind(org_id)
        .bind(user_id)
        .execute(control.pool())
        .await
        .context("写入受保护企业授权事实失败")?;

        ensure!(
            sqlx::query("DELETE FROM users WHERE id = ?")
                .bind(user_id)
                .execute(control.pool())
                .await
                .is_err(),
            "RESTRICT 必须拒绝删除仍被授权事实引用的用户"
        );
        ensure!(
            sqlx::query("UPDATE users SET id = id + 1000000 WHERE id = ?")
                .bind(user_id)
                .execute(control.pool())
                .await
                .is_err(),
            "RESTRICT 必须拒绝修改仍被授权事实引用的用户主键"
        );
        ensure!(
            sqlx::query("DELETE FROM org_org WHERE id = ?")
                .bind(org_id)
                .execute(control.pool())
                .await
                .is_err(),
            "RESTRICT 必须拒绝删除仍被成员事实引用的企业"
        );
        ensure!(
            sqlx::query("UPDATE org_org SET id = id + 1000000 WHERE id = ?")
                .bind(org_id)
                .execute(control.pool())
                .await
                .is_err(),
            "RESTRICT 必须拒绝修改仍被成员事实引用的企业主键"
        );

        sqlx::query(
            "UPDATE `_migrations` SET status = 'running' \
             WHERE module_name = 'yang-system' \
             AND version IN ('20260731_0013_add_admin_user_user_fk', \
                             '20260731_0014_add_org_user_user_fk', \
                             '20260731_0015_add_org_user_org_fk')",
        )
        .execute(control.pool())
        .await
        .context("构造授权外键 DDL 已完成但迁移状态中断失败")?;
        let recovered = run_job(MigrationCommand::Apply).await?;
        ensure!(
            recovered
                .plan
                .entries
                .iter()
                .all(|entry| entry.status == MigrationPlanStatus::Applied),
            "精确完成探针必须恢复三个已完成外键的迁移状态"
        );

        sqlx::query("ALTER TABLE admin_user DROP FOREIGN KEY fk_admin_user_user_user")
            .execute(control.pool())
            .await
            .context("移除正确平台用户外键失败")?;
        sqlx::query(
            "ALTER TABLE admin_user ADD CONSTRAINT fk_admin_user_user_user \
             FOREIGN KEY (user_user) REFERENCES users(id) \
             ON UPDATE RESTRICT ON DELETE CASCADE",
        )
        .execute(control.pool())
        .await
        .context("构造同名异义平台用户外键失败")?;
        sqlx::query(
            "UPDATE `_migrations` SET status = 'running' \
             WHERE module_name = 'yang-system' \
             AND version = '20260731_0013_add_admin_user_user_fk'",
        )
        .execute(control.pool())
        .await
        .context("构造 0013 同名异义 DDL 中断状态失败")?;
        let mismatch = match run_job(MigrationCommand::Apply).await {
            Ok(_) => anyhow::bail!("同名异义外键必须阻止中断恢复"),
            Err(error) => error,
        };
        let mismatch_text = format!("{mismatch:#}");
        ensure!(
            mismatch_text.contains("fk_admin_user_user_user"),
            "完成探针必须拒绝并定位同名异义外键: {mismatch_text}"
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL；staging 应设置 YANG_SYSTEM_AUTHZ_FK_DDL_SCALE_ROWS 与 YANG_SYSTEM_AUTHZ_FK_DDL_BUDGET_MS"]
async fn authorization_foreign_key_ddl_rehearsal_obeys_configured_budget() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        run_job(MigrationCommand::Apply).await?;
        for (table, constraint) in [
            ("admin_user", "fk_admin_user_user_user"),
            ("org_user", "fk_org_user_user_user"),
            ("org_user", "fk_org_user_org_org"),
        ] {
            let statement = format!("ALTER TABLE `{table}` DROP FOREIGN KEY `{constraint}`");
            sqlx::query(&statement)
                .execute(control.pool())
                .await
                .with_context(|| format!("移除外键 {constraint} 以准备 DDL 演练失败"))?;
        }
        sqlx::query(
            "DELETE FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version IN ('20260731_0013_add_admin_user_user_fk', \
                             '20260731_0014_add_org_user_user_fk', \
                             '20260731_0015_add_org_user_org_fk')",
        )
        .execute(control.pool())
        .await
        .context("准备授权外键 DDL 演练迁移起点失败")?;

        let row_count = std::env::var("YANG_SYSTEM_AUTHZ_FK_DDL_SCALE_ROWS")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("YANG_SYSTEM_AUTHZ_FK_DDL_SCALE_ROWS 必须是正整数")?
            .unwrap_or(10_000);
        ensure!(row_count > 0, "授权外键 DDL 演练行数必须大于 0");
        let budget_ms = std::env::var("YANG_SYSTEM_AUTHZ_FK_DDL_BUDGET_MS")
            .ok()
            .map(|value| value.parse::<u128>())
            .transpose()
            .context("YANG_SYSTEM_AUTHZ_FK_DDL_BUDGET_MS 必须是正整数")?
            .unwrap_or(30_000);
        ensure!(budget_ms > 0, "授权外键 DDL 预算必须大于 0ms");

        sqlx::query(
            "INSERT INTO org_org (code, name, status, created_at) \
             VALUES ('fk_scale_org', 'FK Scale Org', 'active', 1)",
        )
        .execute(control.pool())
        .await
        .context("写入授权外键 DDL 演练企业失败")?;
        let org_id: i64 = sqlx::query_scalar("SELECT id FROM org_org WHERE code = 'fk_scale_org'")
            .fetch_one(control.pool())
            .await
            .context("读取授权外键 DDL 演练企业失败")?;

        for batch_start in (0..row_count).step_by(500) {
            let batch_end = (batch_start + 500).min(row_count);
            let mut builder = sqlx::QueryBuilder::<sqlx::MySql>::new(
                "INSERT INTO users (username, password_hash, status, created_at, updated_at) ",
            );
            builder.push_values(batch_start..batch_end, |mut row, index| {
                row.push_bind(format!("fk_scale_{index}"))
                    .push_bind("hash")
                    .push_bind("active")
                    .push_bind(1_i64)
                    .push_bind(1_i64);
            });
            builder
                .build()
                .execute(control.pool())
                .await
                .with_context(|| {
                    format!("写入授权外键 DDL 演练用户失败: {batch_start}..{batch_end}")
                })?;
        }
        sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, admin, status, created_at, updated_at) \
             SELECT id, CONCAT('admin_', id), 0, 'active', 1, 1 \
             FROM users WHERE username LIKE 'fk_scale_%'",
        )
        .execute(control.pool())
        .await
        .context("写入平台授权关系规模数据失败")?;
        sqlx::query(
            "INSERT INTO org_user \
             (org_org, user_user, name, admin, status, created_at, updated_at) \
             SELECT ?, id, CONCAT('member_', id), 0, 'active', 1, 1 \
             FROM users WHERE username LIKE 'fk_scale_%'",
        )
        .bind(org_id)
        .execute(control.pool())
        .await
        .context("写入企业授权关系规模数据失败")?;

        let before_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT CONCAT(TABLE_NAME, ':', INDEX_NAME)) \
             FROM information_schema.statistics \
             WHERE table_schema = DATABASE() \
               AND table_name IN ('admin_user', 'org_user')",
        )
        .fetch_one(control.pool())
        .await
        .context("读取授权外键 DDL 演练前索引规模失败")?;
        let started = Instant::now();
        run_job(MigrationCommand::Apply).await?;
        let elapsed_ms = started.elapsed().as_millis();
        let after_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT CONCAT(TABLE_NAME, ':', INDEX_NAME)) \
             FROM information_schema.statistics \
             WHERE table_schema = DATABASE() \
               AND table_name IN ('admin_user', 'org_user')",
        )
        .fetch_one(control.pool())
        .await
        .context("读取授权外键 DDL 演练后索引规模失败")?;
        ensure!(
            before_indexes == after_indexes,
            "三个授权外键必须复用已有索引，不得隐式增加索引: {before_indexes} -> {after_indexes}"
        );
        ensure!(
            elapsed_ms <= budget_ms,
            "授权外键 DDL 耗时 {elapsed_ms}ms 超过预算 {budget_ms}ms；不得直接发布，应调整发布窗口或采用在线 Schema 变更"
        );
        for constraint in [
            "fk_admin_user_user_user",
            "fk_org_user_user_user",
            "fk_org_user_org_org",
        ] {
            ensure!(
                foreign_key_metadata(&control, constraint).await?.len() == 1,
                "DDL 演练完成后外键 {constraint} 必须精确存在"
            );
        }
        eprintln!(
            "authorization FK DDL rehearsal: rows_per_relation={row_count}, relation_rows={}, indexes={before_indexes}, elapsed_ms={elapsed_ms}, budget_ms={budget_ms}",
            row_count * 2
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL；staging 应设置 YANG_SYSTEM_STATUS_DDL_SCALE_ROWS 与 YANG_SYSTEM_STATUS_DDL_LOCK_BUDGET_MS"]
async fn status_check_ddl_rehearsal_obeys_configured_lock_budget() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        run_job(MigrationCommand::Apply).await?;
        sqlx::query("ALTER TABLE users DROP CHECK chk_users_status")
            .execute(control.pool())
            .await
            .context("移除状态 CHECK 以准备 DDL 演练失败")?;
        sqlx::query(
            "DELETE FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version = '20260731_0012_add_users_status_check'",
        )
        .execute(control.pool())
        .await
        .context("准备 DDL 演练迁移起点失败")?;

        let row_count = std::env::var("YANG_SYSTEM_STATUS_DDL_SCALE_ROWS")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("YANG_SYSTEM_STATUS_DDL_SCALE_ROWS 必须是正整数")?
            .unwrap_or(10_000);
        ensure!(row_count > 0, "DDL 演练行数必须大于 0");
        let budget_ms = std::env::var("YANG_SYSTEM_STATUS_DDL_LOCK_BUDGET_MS")
            .ok()
            .map(|value| value.parse::<u128>())
            .transpose()
            .context("YANG_SYSTEM_STATUS_DDL_LOCK_BUDGET_MS 必须是正整数")?
            .unwrap_or(30_000);
        ensure!(budget_ms > 0, "DDL 锁预算必须大于 0ms");

        for batch_start in (0..row_count).step_by(500) {
            let batch_end = (batch_start + 500).min(row_count);
            let mut builder = sqlx::QueryBuilder::<sqlx::MySql>::new(
                "INSERT INTO users (username, password_hash, status, created_at, updated_at) ",
            );
            builder.push_values(batch_start..batch_end, |mut row, index| {
                row.push_bind(format!("status_scale_{index}"))
                    .push_bind("hash")
                    .push_bind(if index % 2 == 0 { "active" } else { "disabled" })
                    .push_bind(1_i64)
                    .push_bind(1_i64);
            });
            builder
                .build()
                .execute(control.pool())
                .await
                .with_context(|| format!("写入 DDL 演练数据失败: {batch_start}..{batch_end}"))?;
        }
        let before_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT INDEX_NAME) FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'users'",
        )
        .fetch_one(control.pool())
        .await
        .context("读取 DDL 演练前索引规模失败")?;

        let started = Instant::now();
        run_job(MigrationCommand::Apply).await?;
        let elapsed_ms = started.elapsed().as_millis();
        let after_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT INDEX_NAME) FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'users'",
        )
        .fetch_one(control.pool())
        .await
        .context("读取 DDL 演练后索引规模失败")?;
        ensure!(before_indexes == after_indexes, "增加 CHECK 不得改变 users 索引集合");
        ensure!(
            elapsed_ms <= budget_ms,
            "状态 CHECK DDL 耗时 {elapsed_ms}ms 超过预算 {budget_ms}ms；不得直接发布，应改用在线 DDL 或 expand-contract"
        );
        ensure!(
            users_status_check(&control).await?.is_some(),
            "DDL 演练完成后精确 CHECK 必须存在"
        );
        eprintln!(
            "users.status CHECK DDL rehearsal: rows={row_count}, indexes={before_indexes}, elapsed_ms={elapsed_ms}, budget_ms={budget_ms}"
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL；staging 应设置 YANG_SYSTEM_BOOTSTRAP_DDL_SCALE_ROWS 与 YANG_SYSTEM_BOOTSTRAP_DDL_BUDGET_MS"]
#[cfg(any())]
async fn bootstrap_key_check_ddl_rehearsal_obeys_configured_budget() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        run_job(MigrationCommand::Apply).await?;
        sqlx::query("ALTER TABLE admin_user DROP CHECK chk_admin_user_bootstrap_key")
            .execute(control.pool())
            .await
            .context("移除 bootstrap_key CHECK 以准备 DDL 演练失败")?;
        sqlx::query(
            "DELETE FROM `_migrations` WHERE module_name = 'yang-system' \
             AND version = '20260731_0016_add_admin_bootstrap_key_check'",
        )
        .execute(control.pool())
        .await
        .context("准备 bootstrap_key DDL 演练迁移起点失败")?;

        let row_count = std::env::var("YANG_SYSTEM_BOOTSTRAP_DDL_SCALE_ROWS")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("YANG_SYSTEM_BOOTSTRAP_DDL_SCALE_ROWS 必须是正整数")?
            .unwrap_or(10_000);
        ensure!(row_count > 0, "bootstrap_key DDL 演练行数必须大于 0");
        let budget_ms = std::env::var("YANG_SYSTEM_BOOTSTRAP_DDL_BUDGET_MS")
            .ok()
            .map(|value| value.parse::<u128>())
            .transpose()
            .context("YANG_SYSTEM_BOOTSTRAP_DDL_BUDGET_MS 必须是正整数")?
            .unwrap_or(30_000);
        ensure!(budget_ms > 0, "bootstrap_key DDL 预算必须大于 0ms");

        for batch_start in (0..row_count).step_by(500) {
            let batch_end = (batch_start + 500).min(row_count);
            let mut builder = sqlx::QueryBuilder::<sqlx::MySql>::new(
                "INSERT INTO users (username, password_hash, status, created_at, updated_at) ",
            );
            builder.push_values(batch_start..batch_end, |mut row, index| {
                row.push_bind(format!("bootstrap_scale_{index}"))
                    .push_bind("hash")
                    .push_bind("active")
                    .push_bind(1_i64)
                    .push_bind(1_i64);
            });
            builder
                .build()
                .execute(control.pool())
                .await
                .with_context(|| {
                    format!("写入 bootstrap_key DDL 用户失败: {batch_start}..{batch_end}")
                })?;
        }
        sqlx::query(
            "INSERT INTO admin_user \
             (user_user, name, status, admin, bootstrap_key, created_at, updated_at) \
             SELECT id, username, 'active', FALSE, NULL, 1, 1 \
             FROM users WHERE username LIKE 'bootstrap\\_scale\\_%'",
        )
        .execute(control.pool())
        .await
        .context("写入 bootstrap_key DDL 平台授权关系失败")?;
        let actual_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_user")
            .fetch_one(control.pool())
            .await
            .context("读取 bootstrap_key DDL 演练行数失败")?;
        ensure!(
            usize::try_from(actual_rows).ok() == Some(row_count),
            "bootstrap_key DDL 演练行数不完整: expected={row_count}, actual={actual_rows}"
        );

        let before_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT INDEX_NAME) FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'admin_user'",
        )
        .fetch_one(control.pool())
        .await
        .context("读取 bootstrap_key DDL 演练前索引规模失败")?;
        let started = Instant::now();
        run_job(MigrationCommand::Apply).await?;
        let elapsed_ms = started.elapsed().as_millis();
        let after_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT INDEX_NAME) FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = 'admin_user'",
        )
        .fetch_one(control.pool())
        .await
        .context("读取 bootstrap_key DDL 演练后索引规模失败")?;
        ensure!(
            before_indexes == after_indexes,
            "增加 bootstrap_key CHECK 不得改变 admin_user 索引集合"
        );
        ensure!(
            elapsed_ms <= budget_ms,
            "bootstrap_key CHECK DDL 耗时 {elapsed_ms}ms 超过预算 {budget_ms}ms；不得直接发布，应调整发布窗口或采用在线 Schema 变更"
        );
        ensure!(
            bootstrap_key_check(&control).await?.is_some(),
            "bootstrap_key DDL 演练完成后精确 CHECK 必须存在"
        );
        eprintln!(
            "admin_user.bootstrap_key CHECK DDL rehearsal: rows={row_count}, indexes={before_indexes}, elapsed_ms={elapsed_ms}, budget_ms={budget_ms}"
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}
