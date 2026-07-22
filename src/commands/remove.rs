//! `smod remove` — remove an installed package.

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::installer;

/// Arguments for `smod remove`.
#[derive(Args, Debug)]
#[command(
    long_about = "Remove an installed package: delete its smod_modules/ \
                  directory and drop its entries from smod.lock and smod.toml.",
    after_help = "Examples:\n  \
        Remove an installed package:\n    smod remove payment-stream"
)]
pub struct RemoveArgs {
    /// The package to remove.
    pub package: String,
}

/// Entry point for `smod remove`.
pub async fn run(args: RemoveArgs) -> anyhow::Result<()> {
    let project_root = config::require_project_root(std::env::current_dir()?)?;
    installer::remove_package(&project_root, &args.package)?;
    println!("{} {}", "  Removed".green().bold(), args.package.cyan());
    Ok(())
}
