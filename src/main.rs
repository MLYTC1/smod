//! `smod` binary entry point.
//!
//! Parses argv, applies `--no-color`, and dispatches to the right command
//! module. There is no business logic here by design — `dispatch` is the only
//! place that knows all the subcommands exist.

mod cli;
mod commands;
mod config;
mod doctor;
mod installer;
mod lockfile;
mod package;
mod registry;
mod ui;

use clap::Parser;
use colored::Colorize;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if cli.no_color {
        colored::control::set_override(false);
    }

    if let Err(err) = dispatch(cli.command).await {
        eprintln!("{} {:#}", "error:".red().bold(), err);
        std::process::exit(1);
    }
}

/// The single dispatch point mapping each subcommand to its `run`.
async fn dispatch(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Init(args) => commands::init::run(args).await,
        Commands::Install(args) => commands::install::run(args).await,
        Commands::Search(args) => commands::search::run(args).await,
        Commands::Remove(args) => commands::remove::run(args).await,
        Commands::List(args) => commands::list::run(args).await,
        Commands::Publish(args) => commands::publish::run(args).await,
        Commands::Info(args) => commands::info::run(args).await,
        Commands::Doctor(args) => commands::doctor::run(args).await,
        Commands::Update(args) => commands::update::run(args).await,
        Commands::Verify(args) => commands::verify::run(args).await,
    }
}
