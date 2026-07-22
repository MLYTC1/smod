//! The `clap` schema for the whole program, and nothing else.
//!
//! Each subcommand variant wraps that command's own `*Args` struct, which is
//! defined *in the command's own file* — so this file does not become a second
//! place to edit every time a command gains a flag.

use clap::{Parser, Subcommand};

use crate::commands;

/// A cargo/npm-style package manager for Solana modules.
#[derive(Parser, Debug)]
#[command(name = "smod", version, about, long_about = None)]
pub struct Cli {
    /// Enable verbose output. (Parsed today; behavior is a documented no-op.)
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Commands,
}

/// One variant per subcommand.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Create a new smod project in the current directory.
    Init(commands::init::InitArgs),
    /// Install one package, or every declared dependency.
    Install(commands::install::InstallArgs),
    /// Search the registry.
    Search(commands::search::SearchArgs),
    /// Remove an installed package.
    Remove(commands::remove::RemoveArgs),
    /// List declared and installed dependencies.
    List(commands::list::ListArgs),
    /// Publish the current package (not yet implemented).
    Publish(commands::publish::PublishArgs),
    /// Show details about a registry package.
    Info(commands::info::InfoArgs),
    /// Diagnose the local environment (not yet implemented).
    Doctor(commands::doctor::DoctorArgs),
    /// Update installed packages (not yet implemented).
    Update(commands::update::UpdateArgs),
}
