use yang_system::migrations::{parse_cli, print_report, run, USAGE};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let Some(cli) = parse_cli(std::env::args().skip(1)).map_err(|error| anyhow::anyhow!(error))?
    else {
        println!("{USAGE}");
        return Ok(());
    };
    let command = cli.command();
    let report = run(cli).await?;
    print_report(command, &report);
    Ok(())
}
