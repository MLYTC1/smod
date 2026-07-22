//! `smod search` — search the registry.
//!
//! Thin by design: the registry returns structured `Vec<PackageInfo>`; this
//! command only decides how to render it (an aligned table for humans, or the
//! packages serialized as JSON with `--json`).

use clap::Args;
use colored::Colorize;

use crate::commands::OutputArgs;
use crate::registry::{MockRegistryClient, PackageInfo, RegistryClient};
use crate::ui;

/// How wide a description may be before it is truncated in the table view.
const MAX_DESC_WIDTH: usize = 60;

/// Arguments for `smod search`.
#[derive(Args, Debug)]
#[command(
    long_about = "Search the registry for packages whose name or description \
                  matches the query. The match is case-insensitive.",
    after_help = "Examples:\n  \
        Search by keyword:\n    smod search vault\n\n  \
        Search matches descriptions too:\n    smod search payments\n\n  \
        Machine-readable output:\n    smod search payment --json"
)]
pub struct SearchArgs {
    /// The query to match against package names and descriptions.
    pub query: String,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for `smod search`.
pub async fn run(args: SearchArgs) -> anyhow::Result<()> {
    let registry = MockRegistryClient::embedded();
    let results = registry.search(&args.query).await?;

    if args.output.json {
        // Serialize the registry's own structured data directly.
        return ui::json::print(&results);
    }

    if results.is_empty() {
        println!(
            "No packages match {}.",
            format!("\"{}\"", args.query).cyan()
        );
        return Ok(());
    }

    println!("{}", "Found packages:".bold());
    println!();
    print_table(&results);
    Ok(())
}

/// Render the results as an aligned `NAME / VERSION / DESCRIPTION` table.
fn print_table(results: &[PackageInfo]) {
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|p| {
            vec![
                p.name.clone(),
                p.version.clone(),
                truncate(&p.description, MAX_DESC_WIDTH),
            ]
        })
        .collect();
    println!(
        "{}",
        ui::table::render(&["NAME", "VERSION", "DESCRIPTION"], &rows)
    );
}

/// Truncate `text` to at most `max` characters, appending `...` when trimmed.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{kept}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_text_untouched() {
        assert_eq!(truncate("short", 60), "short");
    }

    #[test]
    fn truncate_adds_ellipsis_when_too_long() {
        let out = truncate("abcdefghij", 8);
        assert_eq!(out, "abcde...");
        assert_eq!(out.chars().count(), 8);
    }
}
