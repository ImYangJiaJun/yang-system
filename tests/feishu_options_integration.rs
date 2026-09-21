//! 飞书外部数据源的 Schema 级集成测试。
//!
//! # 覆盖什么
//!
//! 单元测试只能断言 `TableDefinition` 暴露的字段能力位——它不暴露索引，
//! `schema_sync::render` 又是私有模块。因此「`option_id` 真的有唯一索引」这类事实
//! **只能在真实 MySQL 上验证**，那正是本文件存在的理由。
//!
//! # 不覆盖什么
//!
//! HTTP 层的端到端（真打 `POST /api/v1/feishu/approval/options/{source_key}`）需要
//! 完整应用装配（Redis + StepUp 扩展 + JWT keyring），本文件没有搭那套 harness。
//! 那个层次的验证靠 Task 11 的飞书后台联调与前端 e2e。
//!
//! # 运行方式
//!
//! ```text
//! YANG_SYSTEM_TEST_DATABASE_URL=mysql://root:yang-local@127.0.0.1:3306/yang_system_test \
//! YANG_SYSTEM_TEST_REDIS_URL=redis://127.0.0.1:6379/15 \
//! cargo test --test feishu_options_integration -- --ignored --test-threads=1
//! ```
//!
//! 该文件读取 `YANG_SYSTEM_TEST_` 开头的环境变量，因此会被 `scripts/run_ci.py`
//! 的反向发现机制要求登记到 `INTEGRATION_COMMANDS`——不登记 `--self-test` 会失败。

use anyhow::{ensure, Context};
use std::sync::Arc;
use yang_db::Database;

/// 本测试使用的两张表；清理时白名单化，避免拼错表名误删。
const DATASOURCE_TABLE: &str = "feishu_datasource";
const OPTION_TABLE: &str = "feishu_option";

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
        "拒绝在非测试数据库 {name:?} 执行飞书集成测试"
    );
    Ok(database)
}

/// 清理两张飞书表；表名白名单化。
async fn drop_feishu_tables(database: &Database) -> anyhow::Result<()> {
    for table in [OPTION_TABLE, DATASOURCE_TABLE] {
        let statement = match table {
            DATASOURCE_TABLE => "DROP TABLE IF EXISTS `feishu_datasource`",
            OPTION_TABLE => "DROP TABLE IF EXISTS `feishu_option`",
            other => anyhow::bail!("拒绝清理未声明的测试表: {other}"),
        };
        sqlx::query(statement)
            .execute(database.pool())
            .await
            .with_context(|| format!("清理飞书测试表失败: {statement}"))?;
    }
    Ok(())
}

/// 查询某张表上是否存在覆盖指定列的唯一索引。
///
/// schema_sync 的默认索引名规则是 `{uk|idx}_{table}_{fields 用_连接}`，
/// 但显式命名时名字会变——所以按「列 + 唯一性」查而不是按名字查。
async fn has_unique_index_on(
    database: &Database,
    table: &str,
    column: &str,
) -> anyhow::Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT INDEX_NAME) FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME = ? \
           AND NON_UNIQUE = 0 AND INDEX_NAME <> 'PRIMARY'",
    )
    .bind(table)
    .bind(column)
    .fetch_one(database.pool())
    .await
    .context("查询唯一索引失败")?;
    Ok(count > 0)
}

/// 把两张飞书表同步到测试库。
async fn sync_feishu_schema(database: Database) -> anyhow::Result<()> {
    let security = Arc::new(yang_system::config::SecuritySettings::default());
    let config = yang_db::DatabaseConfig::default();
    yang_system::schema::sync_with_database(database, config, security)
        .await
        .context("同步飞书 Schema 失败")?;
    Ok(())
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn feishu_tables_are_created_with_required_unique_indexes() {
    let database = connect_test_database()
        .await
        .unwrap_or_else(|error| panic!("连接测试库失败: {error:#}"));
    drop_feishu_tables(&database)
        .await
        .unwrap_or_else(|error| panic!("预清理失败: {error:#}"));

    let outcome = async {
        // sync_with_database 内部会关掉连接池（tools.close()），且 pool.clone() 共享同一
        // 底层池——所以同步之后必须重新建连，不能复用同步前的句柄。
        sync_feishu_schema(database).await?;
        let handle = connect_test_database().await?;

        // 两张表必须存在
        for table in [DATASOURCE_TABLE, OPTION_TABLE] {
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?",
            )
            .bind(table)
            .fetch_one(handle.pool())
            .await
            .context("查询表存在性失败")?;
            ensure!(exists == 1, "表 {table} 未被创建");
        }

        // 飞书契约要求选项 id「全局唯一且固定」。TableDefinition 不暴露索引，
        // 所以这条只能在这里验证——它是本文件存在的首要理由。
        ensure!(
            has_unique_index_on(&handle, OPTION_TABLE, "option_id").await?,
            "feishu_option.option_id 必须有唯一索引"
        );
        // 数据源标识是路由键，重复会让请求分派歧义
        ensure!(
            has_unique_index_on(&handle, DATASOURCE_TABLE, "source_key").await?,
            "feishu_datasource.source_key 必须有唯一索引"
        );

        // 摘要列必须存在且能容纳 64 位 hex
        let token_hash_len: Option<i64> = sqlx::query_scalar(
            "SELECT CHARACTER_MAXIMUM_LENGTH FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME = 'token_hash'",
        )
        .bind(DATASOURCE_TABLE)
        .fetch_optional(handle.pool())
        .await
        .context("查询 token_hash 列失败")?;
        ensure!(
            token_hash_len.is_some_and(|len| len >= 64),
            "feishu_datasource.token_hash 必须存在且不少于 64 字符"
        );

        // 再次同步必须是无操作：schema_sync 幂等，第二次不应有任何变更。
        // 这里同样会关池，因此断言要在它之前做完。
        drop(handle);
        let second = yang_system::schema::sync_with_database(
            connect_test_database().await?,
            yang_db::DatabaseConfig::default(),
            Arc::new(yang_system::config::SecuritySettings::default()),
        )
        .await
        .context("二次同步失败")?;
        let feishu_changes: Vec<_> = second
            .changes
            .iter()
            .filter(|change| change.table == DATASOURCE_TABLE || change.table == OPTION_TABLE)
            .collect();
        ensure!(
            feishu_changes.is_empty(),
            "二次同步不应再产生飞书表变更，实际: {feishu_changes:?}"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_test_database().await {
        Ok(handle) => drop_feishu_tables(&handle).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("飞书 Schema 集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn option_id_uniqueness_is_enforced_by_the_database() {
    // 双保险：不只断言索引存在，还真的插两次同 id 看数据库是否拒绝。
    // 唯一索引存在但列上有 NULL 或类型不符时，「存在」与「真的生效」是两回事。
    let database = connect_test_database()
        .await
        .unwrap_or_else(|error| panic!("连接测试库失败: {error:#}"));
    drop_feishu_tables(&database)
        .await
        .unwrap_or_else(|error| panic!("预清理失败: {error:#}"));

    let outcome = async {
        sync_feishu_schema(database).await?;
        let handle = connect_test_database().await?;

        sqlx::query(
            "INSERT INTO `feishu_option` (`option_id`, `source_key`, `label`, `sort_order`, `is_default`, `enabled`, `created_at`, `updated_at`) \
             VALUES ('dup_id', 'demo', '第一次', 0, 0, 1, NOW(), NOW())",
        )
        .execute(handle.pool())
        .await
        .context("首次插入应成功")?;

        let second = sqlx::query(
            "INSERT INTO `feishu_option` (`option_id`, `source_key`, `label`, `sort_order`, `is_default`, `enabled`, `created_at`, `updated_at`) \
             VALUES ('dup_id', 'other', '第二次', 1, 0, 1, NOW(), NOW())",
        )
        .execute(handle.pool())
        .await;
        ensure!(
            second.is_err(),
            "重复 option_id 必须被数据库拒绝——飞书要求 id 全局唯一且固定"
        );

        Ok(())
    }
    .await;

    let cleanup = match connect_test_database().await {
        Ok(handle) => drop_feishu_tables(&handle).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("唯一性集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}
