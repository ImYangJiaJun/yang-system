//! 新前端验收专用的无数据库 YANG HTTP 服务。

mod actions;
mod model;
mod modules;
mod view;

use anyhow::Context;
use std::net::SocketAddr;
use std::sync::Arc;
use yang_base::definition::{AddonName, AddonSpec, AppBuilder, ModuleName, ModuleSpec};
use yang_base::tools::ToolsBuilder;
use yang_base::transport::axum::{serve, AxumTransportConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = std::env::var("YANG_DEMO_BIND")
        .unwrap_or_else(|_| "127.0.0.1:18080".to_string())
        .parse::<SocketAddr>()
        .context("YANG_DEMO_BIND 必须是有效 SocketAddr")?;
    let tools = Arc::new(ToolsBuilder::new().build().context("构建空 Tools 失败")?);
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("frontend/e2e/fixtures/report.txt");
    let items = model::fixture_items();
    let api = actions::register_api(
        ModuleSpec::new(ModuleName::new("demo.api").context("Module 名称无效")?),
        fixture,
    )
    .context("注册 demo.api Action 失败")?;
    let category = actions::register_category(modules::category())
        .context("注册 demo.category Action 失败")?;
    let items = actions::register_items(modules::items(), items)
        .context("注册 demo.items Action 失败")?
        .view(view::item_view()?);
    let app = AppBuilder::new()
        .addon(
            AddonSpec::new(AddonName::new("demo").context("Addon 名称无效")?)
                .module(api)
                .module(category)
                .module(items),
        )
        .build(tools)
        .context("构建前端验收应用失败")?;
    serve(bind, Arc::new(app), AxumTransportConfig::default())
        .await
        .context("运行前端验收 HTTP 服务失败")
}
