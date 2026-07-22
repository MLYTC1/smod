//! `smod list` — list declared and installed dependencies.
//!
//! One of the two commands with real branching: it renders declared-vs-installed
//! status (and, for declared+installed packages, whether the installed version
//! satisfies the declared requirement). The branching is presentation only —
//! version-requirement resolution itself lives in [`crate::package`], which this
//! command merely calls.

use std::collections::BTreeSet;

use clap::Args;
use colored::Colorize;
use serde::Serialize;

use crate::commands::OutputArgs;
use crate::config;
use crate::lockfile;
use crate::package::VersionReq;

/// Arguments for `smod list`.
#[derive(Args, Debug)]
#[command(after_help = "Examples:\n  \
        List declared and installed dependencies:\n    smod list\n\n  \
        Machine-readable output:\n    smod list --json")]
pub struct ListArgs {
    #[command(flatten)]
    pub output: OutputArgs,
}

/// The declared-vs-installed status of a single dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DepStatus {
    /// Declared and installed, and the installed version satisfies the
    /// requirement.
    Installed,
    /// Declared and installed, but the installed version does not satisfy the
    /// declared requirement.
    VersionMismatch,
    /// Declared and installed, but the declared requirement could not be parsed.
    InvalidRequirement,
    /// Declared in `smod.toml` but absent from `smod.lock`.
    NotInstalled,
    /// Present in `smod.lock` but not declared in `smod.toml`.
    Untracked,
}

/// One dependency's entry in the machine-readable listing.
#[derive(Debug, Clone, Serialize)]
struct DepEntry {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    declared: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed: Option<String>,
    status: DepStatus,
}

/// Entry point for `smod list`.
pub async fn run(args: ListArgs) -> anyhow::Result<()> {
    let project_root = config::require_project_root(std::env::current_dir()?)?;
    let manifest = config::read_manifest(&project_root)?;
    let lock = lockfile::read(&project_root)?;

    let deps = &manifest.smod.dependencies;

    // The union of every name that is either declared or installed.
    let mut names: BTreeSet<&str> = BTreeSet::new();
    names.extend(deps.keys().map(String::as_str));
    names.extend(lock.packages.iter().map(|p| p.name.as_str()));

    let entries: Vec<DepEntry> = names
        .iter()
        .map(|name| {
            let declared = deps.get(*name).cloned();
            let installed = lock.get(name).map(|p| p.version.clone());
            let status = classify(declared.as_deref(), installed.as_deref());
            DepEntry {
                name: (*name).to_string(),
                declared,
                installed,
                status,
            }
        })
        .collect();

    if args.output.json {
        return crate::ui::json::print(&entries);
    }

    if entries.is_empty() {
        println!("{}", "No dependencies.".dimmed());
        return Ok(());
    }

    println!("Dependencies for {}:", manifest.name.cyan().bold());
    for entry in &entries {
        print_entry(entry);
    }
    Ok(())
}

/// Decide a dependency's [`DepStatus`] from its declared requirement and its
/// installed version. The version-requirement check delegates to
/// [`VersionReq`], keeping resolution logic out of the command.
fn classify(declared: Option<&str>, installed: Option<&str>) -> DepStatus {
    match (declared, installed) {
        (Some(req), Some(version)) => match VersionReq::parse(req) {
            Ok(parsed) if parsed.matches(version) => DepStatus::Installed,
            Ok(_) => DepStatus::VersionMismatch,
            Err(_) => DepStatus::InvalidRequirement,
        },
        (Some(_), None) => DepStatus::NotInstalled,
        (None, Some(_)) => DepStatus::Untracked,
        // Every name originates from `deps` or `lock`, so at least one side is
        // always `Some`.
        (None, None) => unreachable!("name originates from deps or lock"),
    }
}

/// Render one dependency line for the human-readable listing.
fn print_entry(entry: &DepEntry) {
    let name = entry.name.cyan();
    match entry.status {
        DepStatus::Installed => println!(
            "  {} {} {}",
            "✓".green(),
            name,
            version_tag(entry.installed.as_deref()),
        ),
        DepStatus::VersionMismatch => println!(
            "  {} {} {} {}",
            "!".yellow(),
            name,
            version_tag(entry.installed.as_deref()),
            format!(
                "(installed does not satisfy `{}`)",
                entry.declared.as_deref().unwrap_or("")
            )
            .yellow(),
        ),
        DepStatus::InvalidRequirement => println!(
            "  {} {} {} {}",
            "!".yellow(),
            name,
            version_tag(entry.installed.as_deref()),
            format!(
                "(invalid requirement `{}`)",
                entry.declared.as_deref().unwrap_or("")
            )
            .yellow(),
        ),
        DepStatus::NotInstalled => println!(
            "  {} {} {} {}",
            "✗".red(),
            name,
            version_tag(entry.declared.as_deref()),
            "(declared, not installed)".red(),
        ),
        DepStatus::Untracked => println!(
            "  {} {} {} {}",
            "!".yellow(),
            name,
            version_tag(entry.installed.as_deref()),
            "(installed, not declared)".yellow(),
        ),
    }
}

/// Format an optional version as a dimmed `vX.Y.Z` tag.
fn version_tag(version: Option<&str>) -> colored::ColoredString {
    format!("v{}", version.unwrap_or("?")).dimmed()
}
