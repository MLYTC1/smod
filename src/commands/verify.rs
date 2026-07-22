//! `smod verify` — verify installed packages have not been modified.
//!
//! Thin by design: it resolves the project root, builds the embedded registry
//! client, asks [`Installer::verify`] to recompute and compare checksums, and
//! renders the result. All lockfile reading, archive location, and checksum
//! recomputation live in the business layer (`installer.rs`), reusing the same
//! `compute_checksum` used at install time.

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::installer::{Installer, VerificationStatus};
use crate::registry::MockRegistryClient;
use crate::ui;

/// Arguments for `smod verify`.
#[derive(Args, Debug)]
#[command(after_help = "Examples:\n  \
        Verify every installed package:\n    smod verify\n\n\
        Each installed package's archive is re-hashed with SHA-256 and compared\n\
        against the checksum recorded in smod.lock. `verify` exits non-zero if\n\
        any package is missing or its checksum no longer matches.")]
pub struct VerifyArgs {}

/// Entry point for `smod verify`.
pub async fn run(_args: VerifyArgs) -> anyhow::Result<()> {
    let project_root = config::require_project_root(std::env::current_dir()?)?;
    let registry = MockRegistryClient::embedded();
    let installer = Installer::new(&registry, project_root);

    let spinner = ui::spinner::new("verifying installed modules");
    let result = installer.verify().await;
    spinner.finish_and_clear();

    let results = result?;

    println!("{}", "Checking installed modules...".bold());
    println!();

    if results.is_empty() {
        println!("{}", "No installed packages to verify.".dimmed());
        return Ok(());
    }

    let mut failed = 0usize;
    for pkg in &results {
        match &pkg.status {
            VerificationStatus::Verified => {
                println!("{} {}", "✓".green().bold(), pkg.name.cyan());
                println!("  {}", "checksum matches".dimmed());
            }
            VerificationStatus::ChecksumMismatch { .. } => {
                failed += 1;
                println!("{} {}", "✗".red().bold(), pkg.name.cyan());
                println!("  {}", "checksum mismatch".red());
            }
            VerificationStatus::ModuleMissing => {
                failed += 1;
                println!("{} {}", "✗".red().bold(), pkg.name.cyan());
                println!("  {}", "installed module directory is missing".red());
            }
            VerificationStatus::ArchiveUnavailable { reason } => {
                failed += 1;
                println!("{} {}", "✗".red().bold(), pkg.name.cyan());
                println!("  {}", format!("could not verify: {reason}").red());
            }
        }
    }

    println!();
    if failed > 0 {
        // Non-zero exit so scripts and CI can detect a failed verification.
        anyhow::bail!(
            "{failed} of {} package(s) failed verification",
            results.len()
        );
    }
    println!("{}", "All installed packages verified.".green().bold());
    Ok(())
}
