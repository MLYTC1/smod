//! A tiny fixed-width column formatter for human-readable table output.
//!
//! Presentation only, and used exclusively by `commands/*.rs` — like the rest
//! of [`crate::ui`], it does no work and knows nothing about packages. It emits
//! plain (uncolored) text so that column widths are computed from the visible
//! characters, with no ANSI escape codes to throw the alignment off.

/// Render `rows` as a left-aligned, fixed-width table under `headers`.
///
/// Every row is expected to have the same number of cells as `headers`; any
/// extra cells are ignored and any missing ones are treated as empty. Each
/// column is padded to the width of its widest cell (header included), with a
/// two-space gutter between columns and no trailing whitespace on any line.
/// Returns the header line followed by one line per row, joined by newlines
/// (no trailing newline).
pub fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().take(cols).enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(format_row(
        &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
        &widths,
    ));
    for row in rows {
        lines.push(format_row(row, &widths));
    }
    lines.join("\n")
}

/// Format one row: each cell padded to its column width and joined by a
/// two-space gutter. Trailing whitespace is trimmed from the assembled line, so
/// empty or missing trailing cells never leave dangling spaces.
fn format_row(cells: &[String], widths: &[usize]) -> String {
    let mut out = String::new();
    for (i, width) in widths.iter().enumerate() {
        let empty = String::new();
        let cell = cells.get(i).unwrap_or(&empty);
        let pad = width.saturating_sub(cell.chars().count());
        out.push_str(cell);
        out.push_str(&" ".repeat(pad + 2));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns_to_widest_cell() {
        let table = render(
            &["NAME", "VERSION"],
            &[
                vec!["payment-stream".to_string(), "1.0.0".to_string()],
                vec!["nft".to_string(), "0.9.3".to_string()],
            ],
        );
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "NAME            VERSION");
        assert_eq!(lines[1], "payment-stream  1.0.0");
        assert_eq!(lines[2], "nft             0.9.3");
    }

    #[test]
    fn no_trailing_whitespace_on_any_line() {
        let table = render(&["A", "B"], &[vec!["x".to_string(), "y".to_string()]]);
        for line in table.lines() {
            assert_eq!(line, line.trim_end(), "line has trailing space: {line:?}");
        }
    }

    #[test]
    fn missing_cells_are_treated_as_empty() {
        // A short row must not panic; the missing cell renders empty.
        let table = render(&["A", "B"], &[vec!["only".to_string()]]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[1], "only");
    }
}
