use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mode = yang_system::bootstrap::StartupMode::from_args(&args)?;
    yang_system::bootstrap::run_with_mode(Path::new("config.toml"), mode).await
}
