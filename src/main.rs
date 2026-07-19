use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    yang_system::bootstrap::run(Path::new("config.toml")).await
}
