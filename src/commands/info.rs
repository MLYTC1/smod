//! `smod info` — show details about a registry package.
//!
//! Thin by design: the registry returns a structured `PackageInfo`; this
//! command only formats it (a labeled block for humans, or the struct
//! serialized as JSON with `--json`).

use clap::Args;
use colored::Colorize;

use crate::commands::OutputArgs;
use crate::registry::{MockRegistryClient, RegistryClient};
use crate::ui;

/// Width the field labels are padded to, so their values line up.
const LABEL_WIDTH: usize = 13;

/// Arguments for `smod info`.
#[derive(Args, Debug)]
#[command(
    long_about = "Show detailed registry metadata for a single package: its \
                  version, description, author, program id, declared \
                  dependencies, archive location, and checksum.",
    after_help = "Examples:\n  \
        Show details for a package:\n    smod info payment-stream\n\n  \
        Machine-readable output:\n    smod info payment-stream --json"
)]
pub struct InfoArgs {
    /// The package name to look up.
    pub package: String,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for `smod info`.
pub async fn run(args: InfoArgs) -> anyhow::Result<()> {
    let registry = MockRegistryClient::embedded();
    let pkg = registry.get_package(&args.package).await?;

    if args.output.json {
        // Serialize the registry's own structured data directly.
        return ui::json::print(&pkg);
    }

    println!(
        "{} {}",
        pkg.name.cyan().bold(),
        format!("v{}", pkg.version).dimmed()
    );
    field("description", &pkg.description);
    field("author", &pkg.author);
    field("program id", &pkg.program_id);

    // Dependencies: either a comma-free list or an explicit "(none)".
    if pkg.dependencies.is_empty() {
        field_dim("dependencies", "(none)");
    } else {
        println!("  {}", "dependencies".dimmed());
        for (name, req) in &pkg.dependencies {
            println!("    {} {}", name.cyan(), req.dimmed());
        }
    }

    field("archive", &pkg.archive);
    match &pkg.checksum {
        Some(sum) => field("checksum", &format!("sha256:{sum}")),
        None => field_dim("checksum", "(none)"),
    }
    Ok(())
}

/// Print one `  label        value` line, the label dimmed.
fn field(label: &str, value: &str) {
    println!("  {:<LABEL_WIDTH$} {}", label.dimmed(), value);
}

/// Like [`field`], but the value is dimmed too (for placeholder text).
fn field_dim(label: &str, value: &str) {
    println!("  {:<LABEL_WIDTH$} {}", label.dimmed(), value.dimmed());
}
