use anyhow::{ensure, Context};
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;
use yang_db::{Database, DatabaseConfig};
use yang_system::config::SecuritySettings;
use yang_system::migrations::{execute_with_database, MigrationCommand};

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

#[derive(Debug, Clone, Copy)]
enum ProbeStrategy {
    LegacyThreeQueries,
    JoinedCapability,
}

#[derive(Debug)]
struct BenchmarkResult {
    samples_micros: Vec<u128>,
    elapsed_millis: u128,
}

fn database_config() -> DatabaseConfig {
    DatabaseConfig::default()
        .with_max_connections(16)
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
        .context("连接租户查询基准 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取租户查询基准数据库名失败")?;
    let name = name.context("租户查询基准连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行租户查询基准"
    );
    Ok(database)
}

async fn reset_test_database(database: &Database) -> anyhow::Result<()> {
    for table in INTERNAL_TABLES.into_iter().chain(BUSINESS_TABLES) {
        let statement = format!("DROP TABLE IF EXISTS `{table}`");
        sqlx::query(&statement)
            .execute(database.pool())
            .await
            .with_context(|| format!("清理租户查询基准表失败: {table}"))?;
    }
    sqlx::query("DROP TABLE IF EXISTS `_migrations`")
        .execute(database.pool())
        .await
        .context("清理租户查询基准迁移记录失败")?;
    Ok(())
}

async fn apply_schema() -> anyhow::Result<()> {
    execute_with_database(
        MigrationCommand::Apply,
        connect_test_database().await?,
        database_config(),
        security_settings(),
    )
    .await
    .context("准备租户查询基准 Schema 失败")?;
    Ok(())
}

async fn probe(
    pool: &sqlx::MySqlPool,
    strategy: ProbeStrategy,
    user_id: i64,
    org_id: i64,
) -> anyhow::Result<Option<bool>> {
    match strategy {
        ProbeStrategy::LegacyThreeQueries => {
            let membership_exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM org_user \
                 WHERE org_org = ? AND user_user = ? AND status = 'active' LIMIT 1)",
            )
            .bind(org_id)
            .bind(user_id)
            .fetch_one(pool)
            .await
            .context("执行旧租户成员查询失败")?;
            if !membership_exists {
                return Ok(None);
            }
            let organization_exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM org_org \
                 WHERE id = ? AND status = 'active' LIMIT 1)",
            )
            .bind(org_id)
            .fetch_one(pool)
            .await
            .context("执行旧企业状态查询失败")?;
            if !organization_exists {
                return Ok(None);
            }
            let is_admin: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM org_user \
                 WHERE org_org = ? AND user_user = ? \
                   AND status = 'active' AND admin = TRUE LIMIT 1)",
            )
            .bind(org_id)
            .bind(user_id)
            .fetch_one(pool)
            .await
            .context("执行旧企业管理员查询失败")?;
            Ok(Some(is_admin))
        }
        ProbeStrategy::JoinedCapability => sqlx::query_scalar(
            "SELECT membership.admin FROM org_user AS membership \
             INNER JOIN org_org AS organization ON organization.id = membership.org_org \
             WHERE membership.org_org = ? AND membership.user_user = ? \
               AND membership.status = 'active' AND organization.status = 'active' \
             LIMIT 1",
        )
        .bind(org_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .context("执行 JOIN 租户 capability 查询失败"),
    }
}

async fn benchmark_strategy(
    pool: &sqlx::MySqlPool,
    strategy: ProbeStrategy,
    user_id: i64,
    org_id: i64,
    concurrency: usize,
    requests_per_worker: usize,
) -> anyhow::Result<BenchmarkResult> {
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for _ in 0..concurrency {
        let pool = pool.clone();
        tasks.spawn(async move {
            let mut samples = Vec::with_capacity(requests_per_worker);
            for _ in 0..requests_per_worker {
                let request_started = Instant::now();
                let result = probe(&pool, strategy, user_id, org_id).await?;
                ensure!(result == Some(true), "基准正向管理员 capability 必须成立");
                samples.push(request_started.elapsed().as_micros());
            }
            Ok::<_, anyhow::Error>(samples)
        });
    }
    let mut samples_micros = Vec::with_capacity(concurrency * requests_per_worker);
    while let Some(result) = tasks.join_next().await {
        samples_micros.extend(result.context("租户查询基准任务异常结束")??);
    }
    Ok(BenchmarkResult {
        samples_micros,
        elapsed_millis: started.elapsed().as_millis(),
    })
}

fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    samples.sort_unstable();
    let index = (samples.len() * percentile).div_ceil(100).saturating_sub(1);
    samples[index]
}

fn finish_with_cleanup(
    outcome: anyhow::Result<()>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            Err(error.context(format!("租户查询基准失败后清理也失败: {cleanup_error:#}")))
        }
    }
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL；用于比较三查询基线与单 JOIN capability"]
async fn joined_membership_capability_reduces_queries_with_equivalent_security_semantics(
) -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    reset_test_database(&control).await?;

    let outcome = async {
        apply_schema().await?;
        sqlx::query(
            "INSERT INTO org_org (code, name, status, created_at) \
             VALUES ('tenant_bench_org', 'Tenant Benchmark', 'active', 1)",
        )
        .execute(control.pool())
        .await
        .context("写入租户查询基准企业失败")?;
        let org_id: i64 =
            sqlx::query_scalar("SELECT id FROM org_org WHERE code = 'tenant_bench_org'")
                .fetch_one(control.pool())
                .await
                .context("读取租户查询基准企业失败")?;

        let row_count = std::env::var("YANG_SYSTEM_TENANT_BENCHMARK_ROWS")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("YANG_SYSTEM_TENANT_BENCHMARK_ROWS 必须是正整数")?
            .unwrap_or(10_000);
        ensure!(row_count >= 2, "租户查询基准至少需要 2 个成员");
        for batch_start in (0..row_count).step_by(500) {
            let batch_end = (batch_start + 500).min(row_count);
            let mut builder = sqlx::QueryBuilder::<sqlx::MySql>::new(
                "INSERT INTO users (username, password_hash, status, created_at, updated_at) ",
            );
            builder.push_values(batch_start..batch_end, |mut row, index| {
                row.push_bind(format!("tenant_bench_{index}"))
                    .push_bind("hash")
                    .push_bind("active")
                    .push_bind(1_i64)
                    .push_bind(1_i64);
            });
            builder
                .build()
                .execute(control.pool())
                .await
                .with_context(|| format!("写入租户查询基准用户失败: {batch_start}..{batch_end}"))?;
        }
        sqlx::query(
            "INSERT INTO org_user \
             (org_org, user_user, name, admin, status, created_at, updated_at) \
             SELECT ?, id, CONCAT('member_', id), username = ?, 'active', 1, 1 \
             FROM users WHERE username LIKE 'tenant_bench_%'",
        )
        .bind(org_id)
        .bind(format!("tenant_bench_{}", row_count - 1))
        .execute(control.pool())
        .await
        .context("写入租户查询基准成员失败")?;
        let admin_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(format!("tenant_bench_{}", row_count - 1))
            .fetch_one(control.pool())
            .await
            .context("读取租户查询基准管理员失败")?;
        let member_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(format!("tenant_bench_{}", row_count - 2))
            .fetch_one(control.pool())
            .await
            .context("读取租户查询基准普通成员失败")?;

        for strategy in [
            ProbeStrategy::LegacyThreeQueries,
            ProbeStrategy::JoinedCapability,
        ] {
            ensure!(
                probe(control.pool(), strategy, admin_id, org_id).await? == Some(true),
                "管理员正向语义必须一致: {strategy:?}"
            );
            ensure!(
                probe(control.pool(), strategy, member_id, org_id).await? == Some(false),
                "普通成员正向语义必须一致: {strategy:?}"
            );
            ensure!(
                probe(control.pool(), strategy, i64::MAX, org_id).await?.is_none(),
                "非成员必须失败关闭: {strategy:?}"
            );
        }
        sqlx::query("UPDATE org_org SET status = 'disabled' WHERE id = ?")
            .bind(org_id)
            .execute(control.pool())
            .await
            .context("构造 disabled 企业失败")?;
        for strategy in [
            ProbeStrategy::LegacyThreeQueries,
            ProbeStrategy::JoinedCapability,
        ] {
            ensure!(
                probe(control.pool(), strategy, admin_id, org_id).await?.is_none(),
                "disabled 企业必须失败关闭: {strategy:?}"
            );
        }
        sqlx::query("UPDATE org_org SET status = 'active' WHERE id = ?")
            .bind(org_id)
            .execute(control.pool())
            .await
            .context("恢复基准企业状态失败")?;

        for _ in 0..50 {
            probe(
                control.pool(),
                ProbeStrategy::LegacyThreeQueries,
                admin_id,
                org_id,
            )
            .await?;
            probe(
                control.pool(),
                ProbeStrategy::JoinedCapability,
                admin_id,
                org_id,
            )
            .await?;
        }
        let concurrency = 10;
        let requests_per_worker = 100;
        let mut legacy = benchmark_strategy(
            control.pool(),
            ProbeStrategy::LegacyThreeQueries,
            admin_id,
            org_id,
            concurrency,
            requests_per_worker,
        )
        .await?;
        let mut joined = benchmark_strategy(
            control.pool(),
            ProbeStrategy::JoinedCapability,
            admin_id,
            org_id,
            concurrency,
            requests_per_worker,
        )
        .await?;
        let legacy_p50 = percentile(&mut legacy.samples_micros, 50);
        let legacy_p95 = percentile(&mut legacy.samples_micros, 95);
        let legacy_p99 = percentile(&mut legacy.samples_micros, 99);
        let joined_p50 = percentile(&mut joined.samples_micros, 50);
        let joined_p95 = percentile(&mut joined.samples_micros, 95);
        let joined_p99 = percentile(&mut joined.samples_micros, 99);
        ensure!(
            joined_p95 <= legacy_p95.saturating_mul(3) / 2,
            "单 JOIN p95 不得比三查询基线恶化超过 50%: legacy={legacy_p95}us joined={joined_p95}us"
        );
        eprintln!(
            "tenant capability benchmark: rows={row_count}, concurrency={concurrency}, requests={}, legacy_queries_per_request=3, legacy_p50_us={legacy_p50}, legacy_p95_us={legacy_p95}, legacy_p99_us={legacy_p99}, legacy_elapsed_ms={}, joined_queries_per_request=1, joined_p50_us={joined_p50}, joined_p95_us={joined_p95}, joined_p99_us={joined_p99}, joined_elapsed_ms={}",
            concurrency * requests_per_worker,
            legacy.elapsed_millis,
            joined.elapsed_millis
        );
        Ok(())
    }
    .await;

    let cleanup = reset_test_database(&control).await;
    finish_with_cleanup(outcome, cleanup)
}
