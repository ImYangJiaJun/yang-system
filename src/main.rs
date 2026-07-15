use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::var("APP_CONFIG").unwrap_or_else(|_| "config.toml".to_string());
    yang_system::bootstrap::run(Path::new(&config_path)).await
}
