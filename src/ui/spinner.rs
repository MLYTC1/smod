//! An indeterminate spinner, for "doing something that takes an unknown amount
//! of time."

use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

/// Create and start a spinner with the given message.
pub fn new(message: impl Into<String>) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    // `template` only fails on a malformed template literal, which is a
    // compile-time constant here; fall back to the default style if so.
    let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner());
    pb.set_style(style.tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]));
    pb.set_message(message.into());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}
