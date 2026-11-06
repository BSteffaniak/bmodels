//! Terminal output: color (only on a TTY, honoring `NO_COLOR`), tables, and JSON.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::sync::OnceLock;

use bmodels_engine::{Counts, SourceOutcome, SourceReport, SyncReport};
use bmodels_model::EventKind;
use serde::Serialize;

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

/// ANSI styles.
#[derive(Debug, Clone, Copy)]
pub enum Paint {
    Bold,
    Dim,
    Green,
    Yellow,
    Red,
    Cyan,
    Magenta,
}

/// Wraps `text` in `paint` when color is enabled.
pub fn paint(paint: Paint, text: &str) -> String {
    if !color_enabled() {
        return text.to_owned();
    }
    let code = match paint {
        Paint::Bold => "1",
        Paint::Dim => "2",
        Paint::Green => "32",
        Paint::Yellow => "33",
        Paint::Red => "31",
        Paint::Cyan => "36",
        Paint::Magenta => "35",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

/// Color of an event kind.
pub const fn kind_paint(kind: EventKind) -> Paint {
    match kind {
        EventKind::New => Paint::Green,
        EventKind::NewListing => Paint::Cyan,
        EventKind::Changed => Paint::Yellow,
        EventKind::Removed => Paint::Red,
        EventKind::Reappeared => Paint::Magenta,
    }
}

/// Prints `value` as pretty JSON.
pub fn json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// A simple left-aligned table; the last column is never padded.
#[derive(Debug, Default)]
pub struct Table {
    rows: Vec<Vec<(String, Option<Paint>)>>,
}

impl Table {
    /// Adds a row of `(text, style)` cells.
    pub fn row(&mut self, cells: Vec<(String, Option<Paint>)>) {
        self.rows.push(cells);
    }

    /// Prints the table.
    pub fn print(&self) {
        let columns = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..columns)
            .map(|c| {
                self.rows
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|(t, _)| t.chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for row in &self.rows {
            let mut line = String::new();
            for (c, (text, style)) in row.iter().enumerate() {
                let pad = if c + 1 == row.len() {
                    0
                } else {
                    widths[c] - text.chars().count() + 2
                };
                let cell = style.map_or_else(|| text.clone(), |p| paint(p, text));
                line.push_str(&cell);
                line.push_str(&" ".repeat(pad));
            }
            println!("{}", line.trim_end());
        }
    }
}

/// Plain cell.
pub fn cell(text: impl Into<String>) -> (String, Option<Paint>) {
    (text.into(), None)
}

/// Styled cell.
pub fn styled(text: impl Into<String>, paint: Paint) -> (String, Option<Paint>) {
    (text.into(), Some(paint))
}

/// One-line description of counts (`2 new, 1 changed`), or `no changes`.
pub fn counts_summary(c: &Counts) -> String {
    let mut parts = Vec::new();
    for (n, label) in [
        (c.new, "new"),
        (c.new_listings, "new listing(s)"),
        (c.changed, "changed"),
        (c.removed, "removed"),
        (c.reappeared, "reappeared"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {label}"));
        }
    }
    if parts.is_empty() {
        "no changes".to_owned()
    } else {
        parts.join(", ")
    }
}

/// One-line description of a source report.
pub fn source_summary(r: &SourceReport) -> String {
    match &r.outcome {
        SourceOutcome::Synced(c) if c.baseline > 0 && c.new == 0 => {
            format!("{} models (baseline recorded)", c.present)
        }
        SourceOutcome::Synced(c) => format!("{} models, {}", c.present, counts_summary(c)),
        SourceOutcome::NotModified => "not modified".to_owned(),
        SourceOutcome::Skipped(why) => format!("skipped: {why}"),
        SourceOutcome::Failed(error) => format!("failed: {error}"),
    }
}

/// Prints the summary of a sync (per-source lines are printed live as sources finish).
pub fn print_summary(report: &SyncReport, quiet: bool) {
    let failed = report.failures().count();
    let skipped: Vec<&SourceReport> = report
        .sources
        .iter()
        .filter(|r| matches!(r.outcome, SourceOutcome::Skipped(_)))
        .collect();
    let totals = report.totals();
    let synced = report.sources.len() - skipped.len();
    let mut line = format!("synced {synced} source(s): {}", counts_summary(&totals));
    if failed > 0 {
        let _ = write!(line, ", {failed} failed");
    }
    if !skipped.is_empty() && !quiet {
        let mut names: Vec<&str> = skipped.iter().map(|r| r.source.as_str()).collect();
        names.sort_unstable();
        println!(
            "{}",
            paint(
                Paint::Dim,
                &format!(
                    "skipped {} source(s) without an API key: {} (see `bmodels sources`)",
                    skipped.len(),
                    names.join(", ")
                )
            )
        );
    }
    println!(
        "{}",
        paint(
            if failed > 0 {
                Paint::Yellow
            } else {
                Paint::Bold
            },
            &line
        )
    );
    if totals.new > 0 {
        println!(
            "{}",
            paint(
                Paint::Dim,
                "see them with `bmodels new` (or just `bmodels`)"
            )
        );
    }
}
