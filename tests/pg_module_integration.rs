#![allow(clippy::expect_used, clippy::unwrap_used)]
//! 应用层混合模式：新模块声明 PG 具名源、旧模块保持默认 MySQL。
//!
//! 运行：
//! ```text
//! YANG_SYSTEM_TEST_DATABASE_URL=mysql://root:yang-local@127.0.0.1:3306/yang_system_test \
//! YANG_SYSTEM_TEST_POSTGRES_URL=postgres://postgres:postgres@127.0.0.1:55432/test \
//!   cargo test --test pg_module_integration -- --ignored --test-threads=1
//! ```

use std::sync::Arc;
use yang_base::database::sync_table_definitions_grouped;
use yang_base::datasource::DatasourceId;
use yang_base::definition::{
    AddonName, AddonSpec, AppBuilder, Key, ModuleName, ModuleSpec, Str, TableName, TableSpec,
};
use yang_base::tools::ToolsBuilder;
use yang_db::mysql::{Database as MyDatabase, DatabaseConfig as MyConfig};
use yang_db::postgres::{Database as PgDatabase, DatabaseConfig as PgConfig};

fn mysql_url() -> String {
    std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "mysql://root:yang-local@127.0.0.1:3306/yang_system_test".to_string())
}

fn pg_url() -> String {
    std::env::var("YANG_SYSTEM_TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:55432/test".to_string())
}

#[tokio::test]
#[ignore = "需要本地 MySQL 与 PostgreSQL 容器"]
async fn new_module_table_is_created_in_its_pg_datasource_only() {
    let mysql = MyDatabase::connect_with_config(&mysql_url(), MyConfig::default())
        .await
        .expect("连接 MySQL 失败");
    let pg = PgDatabase::connect_with_config(&pg_url(), PgConfig::default())
        .await
        .expect("连接 PG 失败");
    sqlx::query("DROP TABLE IF EXISTS pg_module_probe")
        .execute(mysql.pool())
        .await
        .expect("清理 MySQL 表失败");
    sqlx::query("DROP TABLE IF EXISTS pg_module_probe")
        .execute(pg.pool())
        .await
        .expect("清理 PG 表失败");

    let pg_source = DatasourceId::new("pg_main").expect("数据源名应有效");
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql_named(DatasourceId::default_source(), mysql.clone())
            .postgres_named(pg_source.clone(), pg.clone())
            .default_datasource(DatasourceId::default_source())
            .build()
            .expect("双数据源 Tools 应构建成功"),
    );

    let fields = yang_base::fields! {
        id => Key::new(),
        name => Str::new().require(true),
    };
    let table = TableSpec::new(TableName::new("pg_module_probe").expect("表名应有效"))
        .fields(fields)
        .datasource(pg_source.clone());
    let module =
        ModuleSpec::new(ModuleName::new("pgprobe.items").expect("模块名应有效")).table(table);
    let addon = AddonSpec::new(AddonName::new("pgprobe").expect("Addon 名应有效")).module(module);
    let app = AppBuilder::new()
        .addon(addon)
        .build(Arc::clone(&tools))
        .expect("PG 模块应通过构建期数据源校验");

    let definition = app
        .table_definitions()
        .iter()
        .find(|definition| definition.name() == "pg_module_probe")
        .expect("模块表定义应存在");
    assert_eq!(definition.datasource(), &pg_source);

    let reports = sync_table_definitions_grouped(&tools, &[definition])
        .await
        .expect("按源同步应成功");
    assert_eq!(reports.len(), 1);
    assert!(reports.contains_key(&pg_source));

    let pg_present: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('pg_module_probe')::text")
            .fetch_one(pg.pool())
            .await
            .expect("查询 PG 表失败");
    let mysql_present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = 'pg_module_probe'",
    )
    .fetch_one(mysql.pool())
    .await
    .expect("查询 MySQL 表失败");
    assert_eq!(pg_present.as_deref(), Some("pg_module_probe"));
    assert_eq!(mysql_present, 0);

    sqlx::query("DROP TABLE IF EXISTS pg_module_probe")
        .execute(pg.pool())
        .await
        .expect("清理 PG 表失败");
}
