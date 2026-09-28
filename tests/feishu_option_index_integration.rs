//! # 覆盖什么
//!
//! 出站端点的复合索引 `idx_feishu_option_pick` 必须在真实 MySQL 上真的建出来。
//! 单元测试断言不了索引（`TableDefinition` 不暴露索引），所以只能在真库上验证。
//!
//! 两条路径各一个用例，**都不能少**：
//!
//! - **建表路径**（`pick_index_exists_with_the_declared_columns_in_order`）：
//!   从零建表时索引就该带上。
//! - **升级路径**（`missing_pick_index_is_manually_dropped_then_restored_by_sync`）：
//!   线上 `feishu_option` 是**已有数据的旧表**，本改动的实效取决于同步能否对它执行
//!   `ALTER TABLE ... ADD INDEX`。这条路径建表用例证明不了——若 ALTER 静默失效，
//!   建表用例照样全绿，而部署会在没有索引的情况下跑。
//!
//! # 不覆盖什么
//!
//! - **索引带来的实际提速**。设计 §5.10 的 375–556 ms → 126–278 ms 是在 15 万行
//!   数据集上实测的；本文件只断言索引的**列与顺序**，不建那个量级的数据，也不跑
//!   `EXPLAIN`。计划里这条索引本身是建议项而非硬前提，所以「快了多少」不是门禁。
//! - **建表与唯一索引**。那是 `tests/feishu_options_integration.rs` 的职责；
//!   本文件只关心 `feishu_option` 上这一个复合普通索引。
//!
//! # 依赖
//!
//! 需要 `YANG_SYSTEM_TEST_DATABASE_URL`（库名以 `_test` 结尾）
//! 与 `YANG_SYSTEM_TEST_REDIS_URL`（Redis DB 15）。
//!
//! # 运行方式
//!
//! ```text
//! YANG_SYSTEM_TEST_DATABASE_URL=mysql://root:yang-local@127.0.0.1:3306/yang_system_test \
//! YANG_SYSTEM_TEST_REDIS_URL=redis://127.0.0.1:6379/15 \
//! cargo test --test feishu_option_index_integration -- --ignored --test-threads=1
//! ```
//!
//! 该文件读取 `YANG_SYSTEM_TEST_` 开头的环境变量，因此会被 `scripts/run_ci.py`
//! 的反向发现机制要求登记到 `INTEGRATION`——不登记 `--self-test` 会失败。

use anyhow::{ensure, Context};
use std::sync::Arc;
use yang_db::Database;

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

/// 清理飞书三张表；表名白名单化。
async fn drop_feishu_tables(database: &Database) -> anyhow::Result<()> {
    // 顺序无关：绑定表用的是普通 `Int` + 索引，没有外键约束（设计 §5）。
    for statement in [
        "DROP TABLE IF EXISTS `feishu_option`",
        "DROP TABLE IF EXISTS `feishu_datasource_field`",
        "DROP TABLE IF EXISTS `feishu_datasource`",
    ] {
        sqlx::query(statement)
            .execute(database.pool())
            .await
            .with_context(|| format!("清理飞书测试表失败: {statement}"))?;
    }
    Ok(())
}

/// 把飞书表同步到测试库。
async fn sync_feishu_schema(database: Database) -> anyhow::Result<()> {
    let security = Arc::new(yang_system::config::SecuritySettings::default());
    let config = yang_db::DatabaseConfig::default();
    yang_system::schema::sync_with_database(database, config, security)
        .await
        .context("同步飞书 Schema 失败")?;
    Ok(())
}

/// 按 `SEQ_IN_INDEX` 顺序读出某个索引覆盖的列名。
async fn index_columns(
    database: &Database,
    table: &str,
    index: &str,
) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND INDEX_NAME = ? \
         ORDER BY SEQ_IN_INDEX",
    )
    .bind(table)
    .bind(index)
    .fetch_all(database.pool())
    .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn pick_index_exists_with_the_declared_columns_in_order() {
    let database = connect_test_database()
        .await
        .unwrap_or_else(|error| panic!("连接测试库失败: {error:#}"));
    drop_feishu_tables(&database)
        .await
        .unwrap_or_else(|error| panic!("预清理失败: {error:#}"));

    // 显式标注类型：块里只有 `?` 与 `Ok(())`，没有 `ensure!` 去把错误类型钉成
    // `anyhow::Error`，不标注编译器无法在多个 `impl From<anyhow::Error>` 间选择。
    let outcome: anyhow::Result<()> = async {
        sync_feishu_schema(database).await?;
        // 同步会关池，必须重连（feishu_options_integration.rs 里有这条注释）
        let handle = connect_test_database().await?;
        let columns = index_columns(&handle, "feishu_option", "idx_feishu_option_pick").await?;
        assert_eq!(
            columns,
            vec!["source_key", "enabled", "sort_order", "option_id"],
            "复合索引的列与顺序是设计 §5.10 的前提（前两个等值前缀之后，\
             索引序恰好就是 ORDER BY 的 (sort_order, option_id)）"
        );
        Ok(())
    }
    .await;

    let cleanup = match connect_test_database().await {
        Ok(handle) => drop_feishu_tables(&handle).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("复合索引集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}

/// 只删索引、不删表，然后看同步会不会把它补回来。
///
/// **这条验的是升级路径，不是建表路径。** 上一个用例先 `DROP TABLE` 再同步，索引是在
/// `CREATE TABLE` 时带上的；而线上 `feishu_option` 已存在且没有这个索引，所以真正
/// 起作用的是 `ALTER TABLE ... ADD INDEX`。框架能力在
/// `crates/yang-base/src/database/schema_sync/inspect.rs` 里能读出（它 diff
/// `information_schema.statistics`），但那只是「能力存在」——**这次会不会真的加上**
/// 只能实测。
#[tokio::test]
#[ignore = "需要 YANG_SYSTEM_TEST_DATABASE_URL 与 YANG_SYSTEM_TEST_REDIS_URL"]
async fn missing_pick_index_is_manually_dropped_then_restored_by_sync() {
    let database = connect_test_database()
        .await
        .unwrap_or_else(|error| panic!("连接测试库失败: {error:#}"));
    drop_feishu_tables(&database)
        .await
        .unwrap_or_else(|error| panic!("预清理失败: {error:#}"));

    let outcome: anyhow::Result<()> = async {
        // 1. 先同步一次：建表，索引按声明建好。先确认这个**前置状态**成立，
        //    否则下面「删掉→补回来」的对照不成立。
        sync_feishu_schema(database).await?;
        let handle = connect_test_database().await?;
        let initial = index_columns(&handle, "feishu_option", "idx_feishu_option_pick").await?;
        ensure!(
            initial == ["source_key", "enabled", "sort_order", "option_id"],
            "首次同步后索引就该已按声明建好，实际: {initial:?}"
        );

        // 1b. 塞一行数据，让这张表真的是「已有数据的旧表」——线上场景正是如此。
        //     它同时是一道结构性防线：若同步是靠**重建表**来消除差异（而非
        //     `ALTER TABLE ADD INDEX`），这行数据会消失，症状比「索引没加上」
        //     严重得多（生产数据丢失），必须在这里拦住。
        sqlx::query(
            "INSERT INTO `feishu_option` \
             (`option_id`, `source_key`, `label`, `sort_order`, `is_default`, `enabled`, `created_at`, `updated_at`) \
             VALUES ('upgrade_probe', 'demo', '升级路径探针', 0, 0, 1, NOW(), NOW())",
        )
        .execute(handle.pool())
        .await
        .context("写入升级路径探针行失败")?;

        // 2. 手工删掉索引，模拟「旧库还没有这个索引」的状态。
        sqlx::query("DROP INDEX `idx_feishu_option_pick` ON `feishu_option`")
            .execute(handle.pool())
            .await
            .context("手工 DROP INDEX 失败")?;

        // 3. 断言它**真的没了**。这步是整个用例的地基：没有它，第 5 步可能在验证一个
        //    从未被删掉的索引，用例会退化成永远为真的空断言。
        let dropped = index_columns(&handle, "feishu_option", "idx_feishu_option_pick").await?;
        ensure!(
            dropped.is_empty(),
            "DROP INDEX 没生效，第 5 步将变成空断言，实际仍为: {dropped:?}"
        );

        // 4. 再同步一次。这一步走 `ALTER TABLE ... ADD INDEX`。
        //    同步会关池（tools.close()，且 pool.clone() 共享同一底层池），
        //    所以必须丢掉旧句柄并重新建连——同步前的 handle 到这里已不可用。
        drop(handle);
        sync_feishu_schema(connect_test_database().await?).await?;

        // 5. 索引必须回来，且列与顺序不变。
        let handle = connect_test_database().await?;
        assert_eq!(
            index_columns(&handle, "feishu_option", "idx_feishu_option_pick").await?,
            vec!["source_key", "enabled", "sort_order", "option_id"],
            "已有表缺少该索引时，schema 同步必须按声明补上，且列与顺序不变"
        );

        // 5b. 探针行必须还在：补索引不能以重建表为代价（那会丢生产数据）。
        let survived: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM `feishu_option` WHERE `option_id` = ?")
                .bind("upgrade_probe")
                .fetch_one(handle.pool())
                .await
                .context("查询探针行失败")?;
        assert_eq!(
            survived, 1,
            "补索引不该动数据；探针行消失说明同步走了重建表而不是 ALTER"
        );
        Ok(())
    }
    .await;

    let cleanup = match connect_test_database().await {
        Ok(handle) => drop_feishu_tables(&handle).await,
        Err(error) => Err(error).context("清理用连接失败"),
    };
    if let Err(error) = outcome {
        panic!("索引升级路径集成测试失败: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("清理失败: {error:#}");
    }
}
