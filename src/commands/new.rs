//! `smod new` — create a new project in a new directory.
//!
//! Thin by design: it resolves the current directory, asks
//! [`crate::scaffold::create_project`] to generate the project tree, and prints
//! what was created. All validation and file generation live in the business
//! layer (`scaffold.rs`); this file only renders the typed result.

use clap::Args;
use colored::Colorize;

use crate::scaffold;

/// Arguments for `smod new`.
#[derive(Args, Debug)]
#[command(
    long_about = "Create a new smod project in a new directory: a smod.toml \
                  manifest, a src/lib.rs starter, and a README.md.",
    after_help = "Examples:\n  \
        Create a new module:\n    smod new my-module"
)]
pub struct NewArgs {
    /// The name of the project (also the directory that will be created).
    pub name: String,
}

/// Entry point for `smod new`.
pub async fn run(args: NewArgs) -> anyhow::Result<()> {
    let parent = std::env::current_dir()?;
    let created = scaffold::create_project(&parent, &args.name)?;

    println!(
        "{} smod project {}",
        "Created".green().bold(),
        args.name.cyan()
    );
    for file in &created.files {
        // Show paths relative to where the user ran the command when possible.
        let shown = file.strip_prefix(&parent).unwrap_or(file);
        println!("  {} {}", "+".green(), shown.display());
    }
    println!();
    println!("  {} cd {} && smod install", "next:".dimmed(), args.name);
    Ok(())
}
