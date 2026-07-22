//! `smod update` — update installed packages to the newest registry version.
//!
//! Thin by design: it resolves the project root, builds the embedded registry
//! client, asks [`Installer::update`] to do the work, and renders the result.
//! All version comparison, archive handling, checksum verification, extraction,
//! and lockfile/manifest updates live in the business layer (`installer.rs`),
//! reusing the same verified, atomic install pipeline as `smod install`.

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::installer::{Installer, UpdateOutcome};
use crate::registry::MockRegistryClient;
use crate::ui;

/// Arguments for `smod update`.
#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// A specific package to update. If omitted, all packages are updated.
    pub package: Option<String>,
}

/// Entry point for `smod update`.
pub async fn run(args: UpdateArgs) -> anyhow::Result<()> {
    let project_root = config::require_project_root(std::env::current_dir()?)?;
    let registry = MockRegistryClient::embedded();
    let installer = Installer::new(&registry, project_root);

    let spinner = ui::spinner::new("checking for updates");
    let result = installer.update(args.package.as_deref()).await;
    spinner.finish_and_clear();

    let outcomes = result?;

    // A targeted update against a package that isn't installed: nothing to do,
    // and a clear non-zero failure rather than a silent success.
    if outcomes.is_empty() {
        if let Some(package) = &args.package {
            anyhow::bail!("package `{package}` is not installed");
        }
        println!("{}", "All packages are up to date.".green());
        return Ok(());
    }

    let updated: Vec<&UpdateOutcome> = outcomes
        .iter()
        .filter(|o| matches!(o, UpdateOutcome::Updated { .. }))
        .collect();
    let up_to_date: Vec<&UpdateOutcome> = outcomes
        .iter()
        .filter(|o| matches!(o, UpdateOutcome::UpToDate { .. }))
        .collect();
    let failures: Vec<&UpdateOutcome> = outcomes
        .iter()
        .filter(|o| matches!(o, UpdateOutcome::Failed { .. }))
        .collect();

    // Nothing outdated and nothing failed: everything is current.
    if updated.is_empty() && failures.is_empty() {
        println!("{}", "All packages are up to date.".green());
        return Ok(());
    }

    println!("{}", "Updating packages...".bold());
    println!();

    for outcome in &updated {
        if let UpdateOutcome::Updated { name, from, to } = outcome {
            println!("{}:", name.cyan().bold());
            println!(
                "  {} {} {}",
                from.dimmed(),
                "->".dimmed(),
                to.green().bold()
            );
        }
    }

    // In a mixed run, note the packages that were already current so the report
    // accounts for every dependency (omitted entirely when everything updated).
    for outcome in &up_to_date {
        if let UpdateOutcome::UpToDate { name, version } = outcome {
            println!(
                "{} {} {}",
                "  ↷".dimmed(),
                name.cyan(),
                format!("v{version} (already up to date)").dimmed()
            );
        }
    }

    for outcome in &failures {
        if let UpdateOutcome::Failed { name, error } = outcome {
            println!("{} {}: {}", "  ✗".red(), name.cyan(), error);
        }
    }

    println!();

    if !failures.is_empty() {
        // Non-zero exit so scripts can tell "some failed" from "all fine".
        anyhow::bail!(
            "{} of {} package(s) failed to update",
            failures.len(),
            outcomes.len()
        );
    }

    println!("{}", "Updated successfully.".green().bold());
    Ok(())
}
