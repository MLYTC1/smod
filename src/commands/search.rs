//! `smod search` — search the registry.

use clap::Args;
use colored::Colorize;

use crate::registry::{MockRegistryClient, RegistryClient};

/// Arguments for `smod search`.
#[derive(Args, Debug)]
pub struct SearchArgs {
    /// The query to match against package names and descriptions.
    pub query: String,
}

/// Entry point for `smod search`.
pub async fn run(args: SearchArgs) -> anyhow::Result<()> {
    let registry = MockRegistryClient::embedded();
    let results = registry.search(&args.query).await?;

    if results.is_empty() {
        println!(
            "No packages match {}.",
            format!("\"{}\"", args.query).cyan()
        );
        return Ok(());
    }

    println!(
        "{} package(s) matching {}:",
        results.len(),
        format!("\"{}\"", args.query).cyan()
    );
    for pkg in results {
        println!(
            "  {} {}",
            pkg.name.cyan().bold(),
            format!("v{}", pkg.version).dimmed()
        );
        println!("      {}", pkg.description);
    }
    Ok(())
}
