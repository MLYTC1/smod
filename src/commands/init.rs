//! `smod init` — create a new project in the current directory.

use clap::Args;
use colored::Colorize;

use crate::config;
use crate::package::Manifest;

/// Arguments for `smod init`.
#[derive(Args, Debug)]
pub struct InitArgs {
    /// Package name (defaults to the current directory's name).
    #[arg(long)]
    pub name: Option<String>,
}

/// Create `smod.toml` (and `smod_modules/`) in the current directory.
pub async fn run(args: InitArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;

    if config::is_smod_project(&cwd) {
        anyhow::bail!(
            "a smod project already exists here ({})",
            config::manifest_path_in(&cwd).display()
        );
    }

    let name = match args.name {
        Some(name) => name,
        None => cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "smod-package".to_string()),
    };

    let manifest = Manifest::new(&name);
    config::write_manifest(&cwd, &manifest)?;
    config::ensure_modules_dir(&cwd)?;

    println!(
        "{} smod project {} {}",
        "Created".green().bold(),
        manifest.name.cyan(),
        format!("v{}", manifest.version).dimmed()
    );
    println!(
        "  {} {}",
        "manifest:".dimmed(),
        config::manifest_path_in(&cwd).display()
    );
    Ok(())
}
