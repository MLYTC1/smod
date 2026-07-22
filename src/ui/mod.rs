//! Presentation helpers (spinners, progress bars).
//!
//! Used by `commands/*.rs`, never by business logic — `installer.rs` has no
//! idea a terminal exists. Nothing here does any work; these are purely
//! cosmetic wrappers around `indicatif`.

pub mod json;
pub mod progress;
pub mod spinner;
pub mod table;
