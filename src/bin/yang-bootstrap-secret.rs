use yang_system::modules::admin::bootstrap_secret::generate_bootstrap_secret;

fn main() -> anyhow::Result<()> {
    let generated = generate_bootstrap_secret()?;
    println!("secret={}", generated.secret());
    println!("digest={}", generated.digest().as_str());
    Ok(())
}
