//! The `clap` schema for the whole program, and nothing else.
//!
//! Each subcommand variant wraps that command's own `*Args` struct, which is
//! defined *in the command's own file* — so this file does not become a second
//! place to edit every time a command gains a flag.

use clap::{Parser, Subcommand};

use crate::commands;

/// A cargo/npm-style package manager for Solana modules.
#[derive(Parser, Debug)]
#[command(
    name = "smod",
    version,
    about,
    long_about = "smod installs, removes, and inspects reusable on-chain Solana \
                  modules, tracking them in a human-readable smod.toml manifest \
                  and a smod.lock lockfile.",
    after_help = "Common workflows:\n  \
        smod init --name my-app     Start a new project\n  \
        smod search vault           Discover packages\n  \
        smod install payment-stream Install a package\n  \
        smod verify                 Check installed packages are intact\n  \
        smod doctor                 Diagnose the local environment\n\n\
        Run `smod <command> --help` for command-specific examples."
)]
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
    /// Create a new smod project in a new directory.
    New(commands::new::NewArgs),
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
    /// Diagnose the local project and environment.
    Doctor(commands::doctor::DoctorArgs),
    /// Update installed packages to the newest registry version.
    Update(commands::update::UpdateArgs),
    /// Verify installed packages match their recorded checksums.
    Verify(commands::verify::VerifyArgs),
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Validates the entire clap tree (flags, subcommands, help/after_help
    /// text). Catches misconfigured arguments at test time rather than at
    /// runtime.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    // --- command routing ------------------------------------------------

    #[test]
    fn routes_install_with_package() {
        let cli = Cli::try_parse_from(["smod", "install", "payment-stream"]).unwrap();
        match cli.command {
            Commands::Install(args) => assert_eq!(args.package.as_deref(), Some("payment-stream")),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn routes_install_without_package() {
        let cli = Cli::try_parse_from(["smod", "install"]).unwrap();
        match cli.command {
            Commands::Install(args) => assert!(args.package.is_none()),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn routes_verify_and_doctor() {
        assert!(matches!(
            Cli::try_parse_from(["smod", "verify"]).unwrap().command,
            Commands::Verify(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["smod", "doctor"]).unwrap().command,
            Commands::Doctor(_)
        ));
    }

    #[test]
    fn global_no_color_flag_parses_after_subcommand() {
        let cli = Cli::try_parse_from(["smod", "search", "vault", "--no-color"]).unwrap();
        assert!(cli.no_color);
        assert!(matches!(cli.command, Commands::Search(_)));
    }

    // --- error behavior -------------------------------------------------

    #[test]
    fn missing_required_argument_errors() {
        // `search` requires a query.
        assert!(Cli::try_parse_from(["smod", "search"]).is_err());
    }

    #[test]
    fn unknown_subcommand_errors() {
        assert!(Cli::try_parse_from(["smod", "frobnicate"]).is_err());
    }

    #[test]
    fn no_subcommand_errors() {
        // A subcommand is required.
        assert!(Cli::try_parse_from(["smod"]).is_err());
    }
}
