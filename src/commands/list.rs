//! `smod list` — list declared and installed dependencies.
//!
//! One of the two commands with real branching: it renders declared-vs-installed
//! status. The branching is presentation only.

use std::collections::BTreeSet;

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::lockfile;

/// Arguments for `smod list`.
#[derive(Args, Debug)]
pub struct ListArgs {}

/// Entry point for `smod list`.
pub async fn run(_args: ListArgs) -> anyhow::Result<()> {
    let project_root = config::require_project_root(std::env::current_dir()?)?;
    let manifest = config::read_manifest(&project_root)?;
    let lock = lockfile::read(&project_root)?;

    let deps = &manifest.smod.dependencies;

    // The union of every name that is either declared or installed.
    let mut names: BTreeSet<&str> = BTreeSet::new();
    names.extend(deps.keys().map(String::as_str));
    names.extend(lock.packages.iter().map(|p| p.name.as_str()));

    if names.is_empty() {
        println!("{}", "No dependencies.".dimmed());
        return Ok(());
    }

    println!("Dependencies for {}:", manifest.name.cyan().bold());
    for name in names {
        let declared = deps.get(name);
        let locked = lock.get(name);
        match (declared, locked) {
            (Some(version), Some(_)) => println!(
                "  {} {} {}",
                "✓".green(),
                name.cyan(),
                format!("v{version}").dimmed()
            ),
            (Some(version), None) => println!(
                "  {} {} {} {}",
                "✗".red(),
                name.cyan(),
                format!("v{version}").dimmed(),
                "(declared, not installed)".red()
            ),
            (None, Some(locked)) => println!(
                "  {} {} {} {}",
                "!".yellow(),
                name.cyan(),
                format!("v{}", locked.version).dimmed(),
                "(installed, not declared)".yellow()
            ),
            // A name in `names` came from `deps` or `lock`, so at least one
            // side is always `Some`. This arm is provably unreachable.
            (None, None) => unreachable!("name originates from deps or lock"),
        }
    }
    Ok(())
}
