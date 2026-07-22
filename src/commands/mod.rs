//! The CLI layer: one module per subcommand.
//!
//! Every command follows the same shape — resolve the project root / build a
//! registry client, call into the business logic, then print colored output.
//! Commands never contain business logic, never manage files directly, and
//! never import each other; shared behavior lives in the business-logic layer.

pub mod doctor;
pub mod info;
pub mod init;
pub mod install;
pub mod list;
pub mod publish;
pub mod remove;
pub mod search;
pub mod update;
pub mod verify;
