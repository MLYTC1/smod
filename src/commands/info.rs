//! `smod info` — show details about a registry package.

use clap::Args;
use colored::Colorize;

use crate::registry::{MockRegistryClient, RegistryClient};

/// Arguments for `smod info`.
#[derive(Args, Debug)]
#[command(
    long_about = "Show detailed registry metadata for a single package: its \
                  version, description, author, program id, archive location, \
                  and checksum.",
    after_help = "Examples:\n  \
        Show details for a package:\n    smod info payment-stream"
)]
pub struct InfoArgs {
    /// The package name to look up.
    pub package: String,
}

/// Entry point for `smod info`.
pub async fn run(args: InfoArgs) -> anyhow::Result<()> {
    let registry = MockRegistryClient::embedded();
    let pkg = registry.get_package(&args.package).await?;

    println!(
        "{} {}",
        pkg.name.cyan().bold(),
        format!("v{}", pkg.version).dimmed()
    );
    println!("  {:<12} {}", "description".dimmed(), pkg.description);
    println!("  {:<12} {}", "author".dimmed(), pkg.author);
    println!("  {:<12} {}", "program id".dimmed(), pkg.program_id);
    println!("  {:<12} {}", "archive".dimmed(), pkg.archive);
    match &pkg.checksum {
        Some(sum) => println!("  {:<12} {}", "checksum".dimmed(), sum),
        None => println!("  {:<12} {}", "checksum".dimmed(), "(none)".dimmed()),
    }
    Ok(())
}
