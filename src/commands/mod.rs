//! The CLI layer: one module per subcommand.
//!
//! Every command follows the same shape — resolve the project root / build a
//! registry client, call into the business logic, then print colored output.
//! Commands never contain business logic, never manage files directly, and
//! never import each other; shared behavior lives in the business-logic layer.

use clap::Args;

pub mod doctor;
pub mod info;
pub mod init;
pub mod install;
pub mod list;
pub mod new;
pub mod publish;
pub mod remove;
pub mod search;
pub mod update;
pub mod verify;

/// A reusable output-mode flag, flattened into the commands that support
/// machine-readable output (`list`, `search`, `info`).
///
/// Defining it once here — rather than repeating a `--json` flag in each
/// command — keeps the option's name, help text, and behavior in a single
/// place, mirroring how the global flags live on `Cli`. Human-readable output
/// remains the default; `--json` opts into JSON.
#[derive(Args, Debug, Clone, Copy, Default)]
pub struct OutputArgs {
    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}
