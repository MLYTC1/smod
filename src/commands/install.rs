//! `smod install` — install one package, or every declared dependency.
//!
//! This is one of the two commands with real branching (single-package vs.
//! batch), but the branching is only about *which* business-logic call to make
//! and how to render its result — never new logic.

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::installer::{DependencyOutcome, InstallStage, Installer};
use crate::registry::MockRegistryClient;
use crate::ui;

/// Arguments for `smod install`.
#[derive(Args, Debug)]
#[command(
    long_about = "Install a package from the registry, or install every \
                  dependency declared in smod.toml. Installing updates \
                  smod.toml and smod.lock and extracts the package into \
                  smod_modules/.",
    after_help = "Examples:\n  \
        Install one package:\n    smod install payment-stream\n\n  \
        Install all dependencies declared in smod.toml:\n    smod install"
)]
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
    let installer = Installer::new(registry, project_root);

    // The business layer reports each stage through this callback; deciding how
    // to display them is the command's job, not the installer's.
    let mut on_stage = |stage: InstallStage| println!("{}", stage_line(stage));
    let result = installer
        .install_with_progress(package, &mut on_stage)
        .await;

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

/// Map an [`InstallStage`] to its human-readable progress line.
fn stage_line(stage: InstallStage) -> String {
    let (symbol, text) = match stage {
        InstallStage::Resolving => ("→", "Resolving package"),
        InstallStage::Verifying => ("→", "Verifying checksum"),
        InstallStage::Extracting => ("→", "Extracting archive"),
        InstallStage::UpdatingLockfile => ("→", "Updating lockfile"),
    };
    format!("  {} {}", symbol.cyan(), text.dimmed())
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
