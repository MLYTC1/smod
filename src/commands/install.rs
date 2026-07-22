//! `smod install` — install one package, or every declared dependency.
//!
//! This is one of the two commands with real branching (single-package vs.
//! batch), but the branching is only about *which* business-logic call to make
//! and how to render its result — never new logic.

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::installer::{DependencyOutcome, Installer};
use crate::registry::MockRegistryClient;
use crate::ui;

/// Arguments for `smod install`.
#[derive(Args, Debug)]
pub struct InstallArgs {
    /// The package to install. If omitted, installs every declared dependency.
    pub package: Option<String>,

    /// Install as a dev dependency. (Parsed today; behavior is a no-op.)
    #[arg(long)]
    pub dev: bool,
}

/// Entry point for `smod install`.
pub async fn run(args: InstallArgs) -> anyhow::Result<()> {
    let project_root = config::require_project_root(std::env::current_dir()?)?;
    let registry = MockRegistryClient::embedded();

    match args.package {
        Some(package) => install_single(&registry, project_root, &package).await,
        None => install_all(&registry, project_root).await,
    }
}

async fn install_single(
    registry: &MockRegistryClient,
    project_root: std::path::PathBuf,
    package: &str,
) -> anyhow::Result<()> {
    let spinner = ui::spinner::new(format!("installing {package}"));
    let installer = Installer::new(registry, project_root);
    let result = installer.install_one(package).await;
    spinner.finish_and_clear();

    let installed = result?;
    let short_checksum: String = installed.checksum.chars().take(12).collect();
    println!(
        "{} {} {}",
        "  Installed".green().bold(),
        installed.info.name.cyan(),
        format!("v{}", installed.info.version).dimmed()
    );
    println!(
        "    {} {}",
        "into".dimmed(),
        installed.install_path.display()
    );
    println!("    {} sha256:{}", "checksum".dimmed(), short_checksum);
    Ok(())
}

async fn install_all(
    registry: &MockRegistryClient,
    project_root: std::path::PathBuf,
) -> anyhow::Result<()> {
    let spinner = ui::spinner::new("resolving dependencies");
    let installer = Installer::new(registry, project_root);
    let result = installer.install_all().await;
    spinner.finish_and_clear();

    let outcomes = result?;
    if outcomes.is_empty() {
        println!("{}", "No dependencies to install.".dimmed());
        return Ok(());
    }

    let mut failed = 0usize;
    for outcome in &outcomes {
        match outcome {
            DependencyOutcome::Installed { name, version } => println!(
                "{} {} {}",
                "  ✓ installed".green(),
                name.cyan(),
                format!("v{version}").dimmed()
            ),
            DependencyOutcome::AlreadyInstalled { name, version } => println!(
                "{} {} {}",
                "  ↷ already installed".dimmed(),
                name,
                format!("v{version}").dimmed()
            ),
            DependencyOutcome::Failed { name, error } => {
                failed += 1;
                println!("{} {}: {}", "  ✗ failed".red(), name.cyan(), error);
            }
        }
    }

    if failed > 0 {
        // Non-zero exit so scripts can tell "some failed" from "all fine".
        anyhow::bail!(
            "{failed} of {} dependencies failed to install",
            outcomes.len()
        );
    }
    Ok(())
}
