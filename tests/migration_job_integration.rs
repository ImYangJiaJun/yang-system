use anyhow::{ensure, Context};
use std::collections::BTreeSet;
use std::sync::Arc;
use yang_base::database::MigrationPlanStatus;
use yang_db::{Database, DatabaseConfig};
use yang_system::config::SecuritySettings;
use yang_system::migrations::{execute_with_database, MigrationCommand, MigrationRunReport};

const BUSINESS_TABLES: [&str; 4] = ["org_user", "org_org", "admin_user", "users"];

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
    for table in BUSINESS_TABLES {
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
        ensure!(migration_count == 4, "应记录 4 个 applied 版本");
        sqlx::query(
            "INSERT INTO `users` (`username`, `password_hash`, `status`, `created_at`, `updated_at`) VALUES ('migration_sentinel', 'hash', 'active', 1, 1)",
        )
        .execute(control.pool())
        .await
        .context("写入幂等重跑哨兵失败")?;

        sqlx::query(
            "UPDATE `_migrations` SET status = 'running' WHERE module_name = 'yang-system' AND version = '20260726_0004_create_org_user'",
        )
        .execute(control.pool())
        .await
        .context("模拟迁移作业中断失败")?;
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
