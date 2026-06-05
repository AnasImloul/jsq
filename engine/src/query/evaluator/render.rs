//! Result-row construction and on-demand presentation helpers.
//!
//! `make_result` packages a `Value` into a `QueryResult` with its
//! cached `kind` and `path`; the JSON / preview strings are derived on
//! demand by `write_value_json` and `value_preview`. Keeping the row
//! to a single value representation means every consumer (NDJSON,
//! JSON-array, CSV preview column, FFI marshalling) shares the same
//! serializer — no real-vs-synthetic split that could drift.
//!
//! Number / string formatting helpers (`format_number`, `json_escape`)
//! also live here since they're reused by the evaluator's group-key
//! and fingerprint paths.

use crate::document::{Document, NodeKind};
use crate::path::compute_path;

use super::super::value::Value;
use super::QueryResult;

pub(super) fn make_result(doc: &Document, v: &Value) -> QueryResult {
    QueryResult {
        kind: kind_to_u8(value_kind(doc, v)),
        path: value_path(doc, v),
        value: v.clone(),
    }
}

/// Best-effort `NodeKind` for any `Value`. Real nodes consult the
/// document; synthetic variants map to the conceptually-closest kind
/// so the FFI's `kind` byte / CSV type column stay meaningful.
pub(crate) fn value_kind(doc: &Document, v: &Value) -> NodeKind {
    match v {
        Value::Null => NodeKind::Null,
        Value::Bool(_) => NodeKind::Bool,
        Value::Number(_) => NodeKind::Number,
        Value::Str(_) => NodeKind::String,
        Value::Group { n: Some(_), .. } => NodeKind::Number,
        Value::Group { n: None, .. } => NodeKind::Null,
        Value::GroupList { .. } | Value::Array(_) => NodeKind::Array,
        Value::Object(_) | Value::BucketRow(_) => NodeKind::Object,
        Value::NamedValue { value, .. } => value_kind(doc, value),
        Value::Node(id) => doc.node_kind(*id),
    }
}

/// Per-variant row path. For real nodes the literal JSON pointer; for
/// synthetic rows a label derived from the variant (group key, named
/// item, or a `(synthetic) …` marker that the UI uses to suppress
/// path-based navigation).
fn value_path(doc: &Document, v: &Value) -> String {
    match v {
        Value::Node(id) => String::from_utf8_lossy(&compute_path(doc, *id)).into_owned(),
        Value::Null => "(synthetic) null".into(),
        Value::Bool(b) => format!("(synthetic) {}", if *b { "true" } else { "false" }),
        Value::Number(n) => format!("(synthetic) {}", truncate(&format_number(*n), 64)),
        Value::Str(s) => format!("(synthetic) \"{}\"", truncate(&json_escape(s), 60)),
        Value::Group { key, .. } => key.clone(),
        Value::GroupList { key, .. } => key.clone(),
        Value::Object(fields) => format!("(synthetic) {{ {} keys }}", fields.len()),
        Value::Array(items) => format!("(synthetic) [ {} items ]", items.len()),
        Value::NamedValue { name, .. } => name.clone(),
        Value::BucketRow(fields) => {
            if fields.is_empty() {
                return "(synthetic) { }".into();
            }
            let (_key_name, key_value) = &fields[0];
            let mut buf = String::new();
            write_value_json(&mut buf, doc, key_value);
            // String keys come back quoted; the UI wants the bare label.
            buf.trim_matches('"').to_string()
        }
    }
}

/// Write the JSON encoding of `v` into `out`. Real-node values copy
/// their source bytes verbatim from the mmap (zero-copy for nested
/// containers — this is the property that makes NDJSON output safe
/// to pipe into `jq`). Synthetic values recurse.
pub fn write_value_json(out: &mut String, doc: &Document, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&format_number(*n)),
        Value::Str(s) => {
            out.push('"');
            escape_into(out, s);
            out.push('"');
        }
        Value::Group { n: Some(v), .. } => out.push_str(&format_number(*v)),
        Value::Group { n: None, .. } => out.push_str("null"),
        Value::GroupList { members, .. } => {
            // Bucket of node ids — render as a real JSON array of each
            // member's source JSON. Inlining the members keeps the
            // NDJSON output composable with `jq` without an additional
            // pass.
            out.push('[');
            for (i, id) in members.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_node_json(out, doc, *id);
            }
            out.push(']');
        }
        Value::Object(fields) | Value::BucketRow(fields) => {
            out.push('{');
            for (i, (k, val)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('"');
                escape_into(out, k);
                out.push_str("\": ");
                write_value_json(out, doc, val);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value_json(out, doc, item);
            }
            out.push(']');
        }
        Value::NamedValue { value, .. } => write_value_json(out, doc, value),
        Value::Node(id) => write_node_json(out, doc, *id),
    }
}

fn write_node_json(out: &mut String, doc: &Document, id: u32) {
    match doc.value_bytes(id) {
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => out.push_str(s),
            Err(_) => out.push_str("null"),
        },
        None => out.push_str("null"),
    }
}

/// Short, human-friendly value rendering used for the CSV/TSV preview
/// column and the desktop table view. Containers collapse to a `[N items]`
/// / `{N keys}` placeholder; everything else falls through to the JSON
/// encoding, truncated to `cap` bytes.
pub(crate) fn value_preview(doc: &Document, v: &Value, cap: usize) -> String {
    match v {
        Value::GroupList { members, .. } => {
            let n = members.len();
            format!("[{} {}]", n, if n == 1 { "item" } else { "items" })
        }
        Value::Array(items) => {
            let n = items.len();
            format!("[{} {}]", n, if n == 1 { "item" } else { "items" })
        }
        Value::Object(fields) => {
            let n = fields.len();
            // Use the full synthetic-object JSON, truncated; users
            // scanning a results table want to see the field names.
            let mut buf = String::with_capacity(2 + n * 16);
            write_value_json(&mut buf, doc, v);
            truncate(&buf, cap)
        }
        Value::BucketRow(fields) => {
            // Skip the key field — the row's `path` already shows it.
            let mut buf = String::new();
            buf.push('{');
            for (i, (k, val)) in fields.iter().skip(1).enumerate() {
                if i > 0 {
                    buf.push_str(", ");
                }
                buf.push('"');
                escape_into(&mut buf, k);
                buf.push_str("\": ");
                write_value_json(&mut buf, doc, val);
            }
            buf.push('}');
            truncate(&buf, cap)
        }
        Value::NamedValue { value, .. } => value_preview(doc, value, cap),
        Value::Node(id) => match doc.node_kind(*id) {
            NodeKind::Array | NodeKind::Object => {
                let count = doc.records()[*id as usize].child_count;
                container_preview(doc.node_kind(*id), count)
            }
            _ => {
                let raw = doc
                    .value_bytes(*id)
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                truncate(&raw, cap)
            }
        },
        _ => {
            let mut buf = String::new();
            write_value_json(&mut buf, doc, v);
            truncate(&buf, cap)
        }
    }
}

fn container_preview(kind: NodeKind, count: u32) -> String {
    match kind {
        NodeKind::Object => format!("{{ {} {} }}", count, if count == 1 { "key" } else { "keys" }),
        NodeKind::Array => format!("[ {} {} ]", count, if count == 1 { "item" } else { "items" }),
        _ => String::new(),
    }
}

fn truncate(s: &str, byte_limit: usize) -> String {
    if s.len() <= byte_limit {
        return s.to_string();
    }
    let mut cut = byte_limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + 3);
    out.push_str(&s[..cut]);
    out.push('…');
    out
}

pub(crate) fn kind_to_u8(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Null => 0,
        NodeKind::Bool => 1,
        NodeKind::Number => 2,
        NodeKind::String => 3,
        NodeKind::Array => 4,
        NodeKind::Object => 5,
    }
}

pub(super) fn format_number(n: f64) -> String {
    if n.is_nan() { return "NaN".to_string(); }
    if n.is_infinite() { return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    if n == n.trunc() && n.abs() < 1e16 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    escape_into(&mut out, s);
    out
}

fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
}
