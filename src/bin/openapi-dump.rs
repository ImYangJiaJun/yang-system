//! 导出 Catalog 投影的 OpenAPI 3.1 文档（开发期契约工具，供前端 openapi-typescript 消费）。
//!
//! 用法：`cargo run --locked --bin openapi-dump [输出路径]`；缺省输出到 stdout。
//! 不连接任何真实服务：MySQL 为惰性连接，密钥为本地占位值，进程不监听端口。

use std::io::Write;
use std::sync::Arc;

use anyhow::Context;
use jsonwebtoken::Algorithm;
use sqlx::mysql::MySqlPoolOptions;
use yang_base::definition::OpenApiInfo;
use yang_base::token::TokenManager;
use yang_base::tools::ToolsBuilder;
use yang_db::{Database, DatabaseConfig};
use yang_system::config::SecuritySettings;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = MySqlPoolOptions::new()
        // 元数据导出不发起数据库连接；惰性连接池只为满足 Tools 的资源形态。
        .connect_lazy("mysql://root:unused@127.0.0.1:3306/unused")
        .context("构建惰性 MySQL 连接配置失败")?;
    let mysql =
        Database::from_pool(pool, DatabaseConfig::default()).context("构建 Database 失败")?;
    let tools = Arc::new(
        ToolsBuilder::new()
            .mysql(mysql)
            .token(TokenManager::new_symmetric(
                "openapi-dump-local-placeholder-0000",
                Algorithm::HS256,
                "openapi-dump".to_string(),
                "openapi-dump".to_string(),
                60,
                120,
            ))
            .build()
            .context("构建 Tools 失败")?,
    );
    // 安全参数与 Catalog 投影无关，取与测试一致的本地占位值。
    let security = Arc::new(SecuritySettings {
        argon2_max_concurrency: 1,
        auth_rate_limit_window_seconds: 60,
        auth_rate_limit_ip_attempts: 30,
        auth_rate_limit_username_attempts: 10,
        password_reset_ttl_seconds: 900,
        issue_refresh_credential_version: true,
        trusted_proxy_cidrs: Vec::new(),
    });
    let application =
        yang_system::app::build_metadata_app(tools, security).context("构建应用定义失败")?;
    let document = application
        .runtime
        .catalog()
        .to_openapi(OpenApiInfo::new("yang-system", env!("CARGO_PKG_VERSION")))
        .context("投影 OpenAPI 文档失败")?;
    let json = serde_json::to_string_pretty(&document).context("序列化 OpenAPI 文档失败")?;
    match std::env::args().nth(1) {
        Some(path) => std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("写入 {path} 失败"))?,
        None => writeln!(std::io::stdout(), "{json}").context("输出 OpenAPI 文档失败")?,
    }
    Ok(())
}
