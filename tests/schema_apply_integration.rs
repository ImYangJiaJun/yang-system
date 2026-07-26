use anyhow::{ensure, Context};
use std::sync::Arc;
use std::time::Duration;
use yang_base::database::{DatabaseInitializer, SchemaSyncChangeKind};
use yang_base::table::{Field, Table, TableDefinition};
use yang_db::Database;

const CONCURRENT_TABLE: &str = "b05_schema_concurrent";
const RETRY_TABLE: &str = "b05_schema_retry";

async fn connect_test_database() -> anyhow::Result<Database> {
    let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .context("缺少 YANG_SYSTEM_TEST_DATABASE_URL")?;
    let database = Database::connect(&url)
        .await
        .context("连接测试 MySQL 失败")?;
    let name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(database.pool())
        .await
        .context("读取测试数据库名失败")?;
    let name = name.context("测试连接没有选择数据库")?;
    ensure!(
        name.ends_with("_test"),
        "拒绝在非测试数据库 {name:?} 执行 schema apply 测试"
    );
    Ok(database)
}

async fn drop_test_table(database: &Database, table: &str) -> anyhow::Result<()> {
    let statement = match table {
        CONCURRENT_TABLE => "DROP TABLE IF EXISTS `b05_schema_concurrent`",
        RETRY_TABLE => "DROP TABLE IF EXISTS `b05_schema_retry`",
        _ => anyhow::bail!("拒绝清理未声明的测试表: {table}"),
    };
    sqlx::query(statement)
        .execute(database.pool())
        .await
        .with_context(|| format!("清理 B-05 测试表失败: {statement}"))?;
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
            Err(error.context(format!("测试失败后清理也失败: {cleanup_error:#}")))
        }
    }
}

fn schema_table(name: &str) -> anyhow::Result<TableDefinition> {
    Table::new(name)
        .fields([Field::bigint("id").required().primary_key()])
        .build()
        .map_err(|error| anyhow::anyhow!("构建 {name} 表定义失败: {error}"))
}

fn schema_lock_name(database_name: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in database_name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("yang_base_schema_{hash:016x}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 指向 _test MySQL 数据库"]
async fn concurrent_schema_apply_is_serialized_across_instances() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    drop_test_table(&control, CONCURRENT_TABLE).await?;

    let outcome = async {
        let database_name: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(control.pool())
            .await
            .context("读取 schema lock 数据库名失败")?;
        let lock_name =
            schema_lock_name(&database_name.context("schema lock 控制连接没有选择数据库")?);
        let first_database = connect_test_database().await?;
        let first_pool = first_database.pool().clone();
        let first = DatabaseInitializer::new(first_database, false);
        let first_definition = schema_table(CONCURRENT_TABLE)?;
        let second_database = connect_test_database().await?;
        let second_pool = second_database.pool().clone();
        let second = DatabaseInitializer::new(second_database, false);
        let second_definition = schema_table(CONCURRENT_TABLE)?;
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);
        let mut lock_connection = control
            .pool()
            .acquire()
            .await
            .context("获取 schema lock 控制连接失败")?;
        let acquired: Option<i64> = sqlx::query_scalar("SELECT GET_LOCK(?, 0)")
            .bind(&lock_name)
            .fetch_one(&mut *lock_connection)
            .await
            .context("预持有 schema lock 失败")?;
        ensure!(acquired == Some(1), "控制连接必须预持有 schema lock");

        let first_task = tokio::spawn(async move {
            first_barrier.wait().await;
            first.sync_table_definitions(&[&first_definition]).await
        });
        let second_task = tokio::spawn(async move {
            second_barrier.wait().await;
            second.sync_table_definitions(&[&second_definition]).await
        });
        barrier.wait().await;

        let waiting = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if first_pool.size() > 0
                    && second_pool.size() > 0
                    && first_pool.num_idle() == 0
                    && second_pool.num_idle() == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let wait_check = match waiting {
            Ok(()) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if first_task.is_finished() || second_task.is_finished() {
                    Err(anyhow::anyhow!(
                        "控制连接持锁期间两个实例都必须阻塞在 schema lock"
                    ))
                } else {
                    Ok(())
                }
            }
            Err(_) => Err(anyhow::anyhow!(
                "两个实例未在超时内同时进入 schema lock 等待状态"
            )),
        };

        let release_result: anyhow::Result<Option<i64>> =
            sqlx::query_scalar("SELECT RELEASE_LOCK(?)")
                .bind(&lock_name)
                .fetch_one(&mut *lock_connection)
                .await
                .context("释放预持有的 schema lock 失败");
        let release_check = match release_result {
            Ok(Some(1)) => Ok(()),
            Ok(value) => Err(anyhow::anyhow!(
                "控制连接必须成功释放 schema lock，实际返回: {value:?}"
            )),
            Err(error) => Err(error),
        };
        if let Err(error) = release_check {
            first_task.abort();
            second_task.abort();
            let close_result = lock_connection
                .close()
                .await
                .context("释放锁失败后关闭控制连接也失败");
            let _ = first_task.await;
            let _ = second_task.await;
            if let Err(close_error) = close_result {
                return Err(error.context(format!("同时关闭控制连接失败: {close_error:#}")));
            }
            return Err(error);
        }

        let both_finished = tokio::time::timeout(Duration::from_secs(5), async {
            while !first_task.is_finished() || !second_task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        if both_finished.is_err() {
            first_task.abort();
            second_task.abort();
            let _ = first_task.await;
            let _ = second_task.await;
            anyhow::bail!("释放锁后两个实例未在超时内全部完成");
        }

        let left_result = first_task.await;
        let right_result = second_task.await;
        let left = left_result
            .context("第一个实例任务异常退出")?
            .context("第一个实例 schema apply 失败")?;
        let right = right_result
            .context("第二个实例任务异常退出")?
            .context("第二个实例 schema apply 失败")?;
        wait_check?;

        let created = left
            .changes
            .iter()
            .chain(&right.changes)
            .filter(|change| change.kind == SchemaSyncChangeKind::CreatedTable)
            .count();
        ensure!(created == 1, "并发实例必须只创建一次表，实际 {created}");
        ensure!(
            left.is_noop() ^ right.is_noop(),
            "并发实例应由一个应用变更、另一个在锁后观察到 noop"
        );
        let verifier = DatabaseInitializer::new(connect_test_database().await?, false);
        let definition = schema_table(CONCURRENT_TABLE)?;
        ensure!(
            verifier
                .plan_table_definitions(&[&definition])
                .await
                .context("并发 apply 后规划失败")?
                .is_noop(),
            "并发 apply 后 schema 必须完全对齐"
        );
        Ok(())
    }
    .await;

    let cleanup = drop_test_table(&control, CONCURRENT_TABLE).await;
    finish_with_cleanup(outcome, cleanup)
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 指向 _test MySQL 数据库"]
async fn failed_schema_apply_releases_lock_and_allows_clean_retry() -> anyhow::Result<()> {
    let control = connect_test_database().await?;
    drop_test_table(&control, RETRY_TABLE).await?;

    let outcome = async {
        sqlx::query(
            "CREATE TABLE `b05_schema_retry` (`id` VARCHAR(32) NOT NULL, PRIMARY KEY (`id`))",
        )
        .execute(control.pool())
        .await
        .context("创建不兼容 schema 失败夹具失败")?;
        let definition = schema_table(RETRY_TABLE)?;
        let failed_initializer = DatabaseInitializer::new(connect_test_database().await?, false);

        let error = match failed_initializer
            .sync_table_definitions(&[&definition])
            .await
        {
            Ok(_) => anyhow::bail!("不兼容 schema 必须让第一次 apply 失败"),
            Err(error) => error,
        };
        ensure!(
            error.to_string().contains("不可自动修改"),
            "第一次 apply 应因不兼容结构失败，实际为: {error}"
        );

        sqlx::query("DROP TABLE `b05_schema_retry`")
            .execute(control.pool())
            .await
            .context("修复失败夹具失败")?;
        let retry_initializer = DatabaseInitializer::new(connect_test_database().await?, false);
        let report = retry_initializer
            .sync_table_definitions(&[&definition])
            .await
            .context("另一实例在失败后重跑 schema apply 应成功")?;
        ensure!(
            report
                .changes
                .iter()
                .any(|change| change.kind == SchemaSyncChangeKind::CreatedTable),
            "重跑必须完成缺失表创建"
        );
        ensure!(
            retry_initializer
                .plan_table_definitions(&[&definition])
                .await
                .context("重跑后规划失败")?
                .is_noop(),
            "失败重跑后 schema 必须完全对齐"
        );
        Ok(())
    }
    .await;

    let cleanup = drop_test_table(&control, RETRY_TABLE).await;
    finish_with_cleanup(outcome, cleanup)
}
