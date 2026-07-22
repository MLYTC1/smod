//! Machine-readable JSON output.
//!
//! Presentation only: commands hand this a serializable value (typically a
//! business-layer data structure such as [`PackageInfo`](crate::registry::PackageInfo),
//! serialized as-is) and it prints it as pretty JSON to stdout. Keeping this in
//! [`crate::ui`] means the choice of "human text vs. JSON" is a presentation
//! decision, never something the business layer knows about.

use serde::Serialize;

/// Print `value` to stdout as pretty-printed JSON.
///
/// Serialization can only fail for types whose `Serialize` impl itself errors,
/// which none of `smod`'s plain data structures do; the error is surfaced as an
/// [`anyhow::Error`] so a command can simply `?` on it.
pub fn print<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    println!("{text}");
    Ok(())
}
