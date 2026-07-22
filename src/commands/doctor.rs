//! `smod doctor` — diagnose the local project and environment.
//!
//! Thin by design: it builds the embedded registry client, asks
//! [`crate::doctor::diagnose`] to run every check, and renders the resulting
//! [`DoctorReport`](crate::doctor::DoctorReport). All detection, filesystem,
//! and verification logic lives in the business layer (`doctor.rs`,
//! `installer.rs`); this file only formats output and maps the result to an
//! exit code.

use clap::Args;
use colored::Colorize;

use crate::doctor::{self, CheckStatus};
use crate::registry::MockRegistryClient;

/// Arguments for `smod doctor`.
#[derive(Args, Debug)]
#[command(after_help = "Examples:\n  \
        Diagnose the current project:\n    smod doctor\n\n\
        `doctor` runs a series of checks (project layout, manifest, lockfile,\n\
        registry availability, installed modules, and checksums) and exits\n\
        non-zero if any check fails.")]
pub struct DoctorArgs {}

/// Entry point for `smod doctor`.
pub async fn run(_args: DoctorArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let registry = MockRegistryClient::embedded();

    let report = doctor::diagnose(&cwd, &registry).await;

    println!("{}", "Running smod diagnostics...".bold());
    println!();
    for check in &report.checks {
        match check.status {
            CheckStatus::Success => println!(
                "{} {}",
                "  ✓".green().bold(),
                format!("{}: {}", check.name, check.message).dimmed()
            ),
            CheckStatus::Failure => println!(
                "{} {} {}",
                "  ✗".red().bold(),
                format!("{}:", check.name).cyan(),
                check.message.red()
            ),
        }
    }
    println!();

    if report.is_healthy() {
        println!("{}", "Everything looks good.".green().bold());
        Ok(())
    } else {
        // Non-zero exit so scripts and CI can detect an unhealthy project.
        anyhow::bail!(
            "doctor found {} problem(s) — see the failed check(s) above",
            report.failure_count()
        )
    }
}
