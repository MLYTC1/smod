//! A determinate byte-progress bar.
//!
//! Styled and ready for the day archive downloads are streamed over HTTP (see
//! the HTTP section of `ARCHITECTURE.md`). Currently unused for that reason —
//! and that's fine.

use indicatif::{ProgressBar, ProgressStyle};

/// Create a byte-oriented progress bar sized to `total` bytes.
///
/// Currently unused: it is built and styled ahead of the HTTP download support
/// described in `ARCHITECTURE.md`, where a streamed body will drive it.
#[allow(dead_code)]
pub fn new(total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    let style = ProgressStyle::with_template(
        "{spinner:.cyan} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta})",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("=>-");
    pb.set_style(style);
    pb
}
