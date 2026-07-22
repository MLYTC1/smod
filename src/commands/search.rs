//! `smod search` — search the registry.

use clap::Args;
use colored::Colorize;

use crate::registry::{MockRegistryClient, RegistryClient};

/// Arguments for `smod search`.
#[derive(Args, Debug)]
#[command(
    long_about = "Search the registry for packages whose name or description \
                  matches the query. The match is case-insensitive.",
    after_help = "Examples:\n  \
        Search by keyword:\n    smod search vault\n\n  \
        Search matches descriptions too:\n    smod search payments"
)]
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
