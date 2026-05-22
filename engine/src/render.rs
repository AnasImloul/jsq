//! Output formatters for query results — the single source of truth
//! both the Swift app's export menu and the `jsq` CLI delegate to.
//!
//! Each renderer takes the same `(results, &Document)` pair and produces
//! a UTF-8 string in the chosen format. Real-node value lookups go
//! through `Document::value_bytes` so the JSON output is byte-for-byte
//! identical to the source mmap for nodes that came straight from the
//! file; synthetic values (aggregate outputs, projections, group lists)
//! fall back to the result's stored `full_text` / `preview`.

use crate::document::{Document, NULL_NODE};
use crate::query::evaluator::QueryResult;

/// One row's "value" as a JSON-encoded string. Real nodes come back
/// zero-copy from the mmap; synthetic rows fall back to the canonical
/// text the evaluator stored.
fn value_json<'a>(r: &'a QueryResult, doc: &'a Document) -> &'a str {
    if r.node_id != NULL_NODE {
        if let Some(bytes) = doc.value_bytes(r.node_id) {
            if let Ok(s) = std::str::from_utf8(bytes) {
                return s;
            }
        }
    }
    if !r.full_text.is_empty() {
        &r.full_text
    } else {
        &r.preview
    }
}

/// `ndjson` — one JSON value per line. Pipe-friendly default for
/// non-TTY stdout.
pub fn render_ndjson(results: &[QueryResult], doc: &Document) -> String {
    let mut out = String::with_capacity(results.len() * 64);
    for r in results {
        out.push_str(value_json(r, doc));
        out.push('\n');
    }
    out
}

/// `json` — one top-level array, pretty-printed two-space indent.
pub fn render_json_array(results: &[QueryResult], doc: &Document) -> String {
    let mut out = String::with_capacity(results.len() * 64);
    out.push_str("[\n");
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  ");
        out.push_str(value_json(r, doc));
    }
    out.push_str("\n]\n");
    out
}

/// `csv` — RFC-4180 quoted, `path,type,preview` columns. The preview
/// is the table-friendly rendering the engine produced; consumers that
/// need the full value should use `ndjson` instead.
pub fn render_csv(results: &[QueryResult]) -> String {
    let mut out = String::with_capacity(results.len() * 64);
    out.push_str("path,type,preview\n");
    for r in results {
        write_csv_field(&mut out, &r.path);
        out.push(',');
        out.push_str(kind_label(r.kind));
        out.push(',');
        write_csv_field(&mut out, &r.preview);
        out.push('\n');
    }
    out
}

/// `tsv` — tab-separated, no quoting. Tabs / newlines inside values
/// become spaces (TSV has no canonical escape mechanism, so we
/// sanitize rather than introduce a dialect).
pub fn render_tsv(results: &[QueryResult]) -> String {
    let mut out = String::with_capacity(results.len() * 64);
    out.push_str("path\ttype\tpreview\n");
    for r in results {
        push_sanitized_tsv(&mut out, &r.path);
        out.push('\t');
        out.push_str(kind_label(r.kind));
        out.push('\t');
        push_sanitized_tsv(&mut out, &r.preview);
        out.push('\n');
    }
    out
}

/// `table` — human-readable two-column grid. `ascii=true` uses plain
/// `+--+` separators so the output survives `tee`/`less -S` cleanly;
/// `false` uses Unicode box-drawing for a tidier TTY look.
///
/// The Path column is capped to `path_cap` characters, the Value to
/// `value_cap`; oversized cells get an ellipsis suffix. Cap defaults
/// (40 / 100) work for an 80-col terminal; callers with a measured
/// terminal width should pass tighter caps.
pub fn render_table(
    results: &[QueryResult],
    doc: &Document,
    ascii: bool,
    path_cap: usize,
    value_cap: usize,
) -> String {
    if results.is_empty() {
        return String::new();
    }

    // Pre-compute every cell so the width calculation only walks each
    // string once.
    let rows: Vec<(String, String)> = results
        .iter()
        .map(|r| {
            let value = value_json(r, doc).to_string();
            (truncate(&r.path, path_cap), truncate(&value, value_cap))
        })
        .collect();

    let path_w = rows
        .iter()
        .map(|(p, _)| display_width(p))
        .max()
        .unwrap_or(0)
        .max(display_width("path"));
    let value_w = rows
        .iter()
        .map(|(_, v)| display_width(v))
        .max()
        .unwrap_or(0)
        .max(display_width("value"));

    let glyphs = if ascii {
        BoxGlyphs::ASCII
    } else {
        BoxGlyphs::UNICODE
    };

    let mut out = String::new();
    write_horizontal(&mut out, glyphs.top_left, glyphs.top_t, glyphs.top_right, glyphs.h, path_w, value_w);
    write_row(&mut out, glyphs.v, "path", "value", path_w, value_w);
    write_horizontal(&mut out, glyphs.left_t, glyphs.cross, glyphs.right_t, glyphs.h, path_w, value_w);
    for (p, v) in &rows {
        write_row(&mut out, glyphs.v, p, v, path_w, value_w);
    }
    write_horizontal(&mut out, glyphs.bottom_left, glyphs.bottom_t, glyphs.bottom_right, glyphs.h, path_w, value_w);
    out
}

fn write_horizontal(
    out: &mut String,
    left: char,
    mid: char,
    right: char,
    fill: char,
    path_w: usize,
    value_w: usize,
) {
    out.push(left);
    for _ in 0..(path_w + 2) {
        out.push(fill);
    }
    out.push(mid);
    for _ in 0..(value_w + 2) {
        out.push(fill);
    }
    out.push(right);
    out.push('\n');
}

fn write_row(out: &mut String, sep: char, left: &str, right: &str, path_w: usize, value_w: usize) {
    out.push(sep);
    out.push(' ');
    out.push_str(left);
    pad(out, path_w - display_width(left));
    out.push(' ');
    out.push(sep);
    out.push(' ');
    out.push_str(right);
    pad(out, value_w - display_width(right));
    out.push(' ');
    out.push(sep);
    out.push('\n');
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

/// Char-count based width — good enough for the ASCII-heavy paths and
/// JSON values the engine produces. Doesn't handle CJK double-width
/// or zero-width joiners; users with such data should prefer `ndjson`.
fn display_width(s: &str) -> usize {
    s.chars().count()
}

fn truncate(s: &str, cap: usize) -> String {
    if display_width(s) <= cap {
        return s.to_string();
    }
    // Reserve one character for the ellipsis.
    let keep = cap.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

struct BoxGlyphs {
    top_left: char,
    top_t: char,
    top_right: char,
    left_t: char,
    cross: char,
    right_t: char,
    bottom_left: char,
    bottom_t: char,
    bottom_right: char,
    h: char,
    v: char,
}

impl BoxGlyphs {
    const UNICODE: Self = Self {
        top_left: '┌',
        top_t: '┬',
        top_right: '┐',
        left_t: '├',
        cross: '┼',
        right_t: '┤',
        bottom_left: '└',
        bottom_t: '┴',
        bottom_right: '┘',
        h: '─',
        v: '│',
    };
    const ASCII: Self = Self {
        top_left: '+',
        top_t: '+',
        top_right: '+',
        left_t: '+',
        cross: '+',
        right_t: '+',
        bottom_left: '+',
        bottom_t: '+',
        bottom_right: '+',
        h: '-',
        v: '|',
    };
}

fn write_csv_field(out: &mut String, s: &str) {
    let needs_quoting =
        s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if !needs_quoting {
        out.push_str(s);
        return;
    }
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

fn push_sanitized_tsv(out: &mut String, s: &str) {
    for c in s.chars() {
        if c == '\t' || c == '\n' || c == '\r' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
}

fn kind_label(kind: u8) -> &'static str {
    match kind {
        0 => "null",
        1 => "bool",
        2 => "number",
        3 => "string",
        4 => "array",
        5 => "object",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::evaluator::QueryResult;

    fn r(path: &str, kind: u8, value: &str) -> QueryResult {
        QueryResult {
            node_id: NULL_NODE,
            kind,
            path: path.into(),
            preview: value.into(),
            full_text: value.into(),
        }
    }

    /// `cargo test` runs tests in parallel by default, so each helper
    /// caller gets its own temp-file path. A shared path would race.
    fn dummy_doc(test_name: &str) -> Document {
        use std::io::Write;
        let path = std::env::temp_dir()
            .join(format!("engine_render_dummy_{}.json", test_name));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"{}").unwrap();
        Document::open(&path, None).unwrap()
    }

    #[test]
    fn ndjson_one_per_line() {
        let doc = dummy_doc("ndjson");
        let rows = vec![r(".a", 2, "1"), r(".b", 2, "2"), r(".c", 2, "3")];
        assert_eq!(render_ndjson(&rows, &doc), "1\n2\n3\n");
    }

    #[test]
    fn json_array_uses_two_space_indent() {
        let doc = dummy_doc("json_array");
        let rows = vec![r(".a", 2, "1"), r(".b", 2, "2")];
        assert_eq!(render_json_array(&rows, &doc), "[\n  1,\n  2\n]\n");
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        let rows = vec![
            r("plain", 3, "value"),
            r("has,comma", 3, "has\"quote"),
            r("nl", 3, "two\nlines"),
        ];
        let out = render_csv(&rows);
        assert!(out.starts_with("path,type,preview\n"));
        assert!(out.contains("plain,string,value\n"));
        assert!(out.contains("\"has,comma\",string,\"has\"\"quote\""));
        assert!(out.contains("\"two\nlines\""));
    }

    #[test]
    fn tsv_replaces_special_chars_with_spaces() {
        let rows = vec![r("a\tb", 3, "x\ny")];
        let out = render_tsv(&rows);
        assert!(out.contains("a b\tstring\tx y"));
    }

    #[test]
    fn table_unicode_columns_line_up() {
        let doc = dummy_doc("table_unicode");
        let rows = vec![r(".x", 2, "1"), r(".longerpath", 2, "42")];
        let out = render_table(&rows, &doc, /* ascii */ false, 40, 100);
        // Headers + every data row should share the same printed width.
        let widths: std::collections::HashSet<usize> =
            out.lines().map(|l| l.chars().count()).collect();
        assert_eq!(widths.len(), 1, "uneven row widths in:\n{}", out);
        assert!(out.contains("┌") && out.contains("│"));
    }

    #[test]
    fn table_ascii_avoids_box_drawing() {
        let doc = dummy_doc("table_ascii");
        let rows = vec![r(".x", 2, "1")];
        let out = render_table(&rows, &doc, /* ascii */ true, 40, 100);
        assert!(out.contains("+") && out.contains("|"));
        assert!(!out.contains("│"));
    }

    #[test]
    fn table_truncates_long_cells_with_ellipsis() {
        let doc = dummy_doc("table_truncate");
        let rows = vec![r(&"a".repeat(60), 3, &"b".repeat(60))];
        let out = render_table(&rows, &doc, true, 10, 12);
        assert!(out.contains("aaaaaaaaa…"));
        assert!(out.contains("bbbbbbbbbbb…"));
    }
}
