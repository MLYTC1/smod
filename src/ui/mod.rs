//! Presentation helpers (color decisions, spinners, progress bars).
//!
//! Used by `commands/*.rs` (and, for [`color`], the binary entry point), never
//! by business logic — `installer.rs` has no idea a terminal exists. Nothing
//! here does any real work; these are cosmetic wrappers plus the single place
//! that decides whether colored output is enabled.

pub mod color;
pub mod json;
pub mod progress;
pub mod spinner;
pub mod table;
