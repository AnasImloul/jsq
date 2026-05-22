//! Result-row rendering. Translates a `Value` (engine node, synthetic
//! literal, group bucket, projection object, …) into the
//! path/preview/full_text triple that crosses the FFI boundary.
//!
//! Number / string formatting helpers (`format_number`, `json_escape`)
//! also live here since they're reused by the evaluator's group-key
//! and fingerprint paths.

use crate::document::{Document, NodeKind, NULL_NODE};
use crate::path::compute_path;

use super::super::value::Value;
use super::QueryResult;

pub(super) fn make_result(doc: &Document, v: &Value) -> QueryResult {
    match v {
        Value::Node(id) => {
            let kind = doc.node_kind(*id);
            let path = String::from_utf8_lossy(&compute_path(doc, *id)).into_owned();
            match kind {
                NodeKind::Array | NodeKind::Object => {
                    let count = doc.records()[*id as usize].child_count;
                    let preview = container_preview(kind, count);
                    QueryResult {
                        node_id: *id,
                        kind: kind_to_u8(kind),
                        path,
                        preview,
                        full_text: String::new(),
                    }
                }
                _ => {
                    let raw_bytes = doc.value_bytes(*id).unwrap_or(&[]);
                    let raw = String::from_utf8_lossy(raw_bytes).into_owned();
                    let preview = truncate(&raw, 80);
                    QueryResult {
                        node_id: *id,
                        kind: kind_to_u8(kind),
                        path,
                        preview,
                        full_text: raw,
                    }
                }
            }
        }
        Value::Null => synthetic_result("null", NodeKind::Null),
        Value::Bool(b) => {
            let s = if *b { "true" } else { "false" };
            synthetic_result(s, NodeKind::Bool)
        }
        Value::Number(n) => synthetic_result(&format_number(*n), NodeKind::Number),
        Value::Str(s) => {
            let q = format!("\"{}\"", json_escape(s));
            synthetic_result(&q, NodeKind::String)
        }
        Value::Group { key, n } => {
            let (formatted, kind) = match n {
                Some(v) => (format_number(*v), NodeKind::Number),
                None => ("null".to_string(), NodeKind::Null),
            };
            QueryResult {
                node_id: NULL_NODE,
                kind: kind_to_u8(kind),
                path: key.clone(),
                preview: truncate(&formatted, 80),
                full_text: formatted,
            }
        }
        Value::GroupList { key, members } => {
            let count = members.len();
            let label = if count == 1 { "item" } else { "items" };
            let preview = format!("[{} {}]", count, label);
            // Click-through: select the first member so the inspector
            // jumps into the group; the result row's path advertises
            // the group key.
            QueryResult {
                node_id: members.first().copied().unwrap_or(NULL_NODE),
                kind: kind_to_u8(NodeKind::Array),
                path: key.clone(),
                preview,
                full_text: format!("{}: {} {}", key, count, label),
            }
        }
        Value::Object(fields) => {
            let json = render_synthetic_object(doc, fields);
            QueryResult {
                node_id: NULL_NODE,
                kind: kind_to_u8(NodeKind::Object),
                path: format!("(synthetic) {{ {} keys }}", fields.len()),
                preview: truncate(&json, 80),
                full_text: json,
            }
        }
        Value::NamedValue { name, value } => {
            // Output of an aggregate-no-by item whose value isn't a
            // scalar (object-valued aggregates, group lists). The
            // `name` is the user-written item name; surfacing it as
            // the row's path and the inner value's JSON in `full_text`
            // lets the UI render the row as a labeled top-level entry
            // instead of an opaque synthetic wrapper.
            let json = render_value_inline(doc, value);
            QueryResult {
                node_id: NULL_NODE,
                kind: kind_to_u8(inner_kind(value)),
                path: name.clone(),
                preview: truncate(&json, 80),
                full_text: json,
            }
        }
        // Aggregate-by row. First field is the group key — surface
        // its rendered value as the row's path. The remaining fields
        // are the reductions, rendered as a JSON object in the value
        // column. Empty bucket rows (shouldn't happen — the
        // aggregate emits at least the key) fall back to the
        // generic Object preview.
        Value::BucketRow(fields) => {
            if fields.is_empty() {
                return QueryResult {
                    node_id: NULL_NODE,
                    kind: kind_to_u8(NodeKind::Object),
                    path: "(synthetic) { }".into(),
                    preview: "{}".into(),
                    full_text: "{}".into(),
                };
            }
            let (_key_name, key_value) = &fields[0];
            let path = render_value_inline(doc, key_value);
            // Strip wrapping quotes on string keys for a cleaner
            // path label — the row is "day", not `"day"`.
            let path = path.trim_matches('"').to_string();
            let json = render_synthetic_object(doc, &fields[1..]);
            QueryResult {
                node_id: NULL_NODE,
                kind: kind_to_u8(NodeKind::Object),
                path,
                preview: truncate(&json, 80),
                full_text: json,
            }
        }
    }
}

/// Renders a synthetic object as a single-line JSON string. Used for
/// preview/full_text on result rows produced by surface `select { ... }`
/// projections. Field order is preserved from the AST.
fn render_synthetic_object(doc: &Document, fields: &[(String, Value)]) -> String {
    let mut out = String::with_capacity(2 + fields.len() * 16);
    out.push('{');
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('"');
        out.push_str(&json_escape(k));
        out.push_str("\": ");
        out.push_str(&render_value_inline(doc, v));
    }
    out.push('}');
    out
}

fn render_value_inline(doc: &Document, v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => if *b { "true".into() } else { "false".into() },
        Value::Number(n) => format_number(*n),
        Value::Str(s) => format!("\"{}\"", json_escape(s)),
        Value::Group { n: Some(v), .. } => format_number(*v),
        Value::Group { n: None, .. } => "null".into(),
        Value::GroupList { members, .. } => format!("[{} items]", members.len()),
        Value::Object(fs) | Value::BucketRow(fs) => render_synthetic_object(doc, fs),
        Value::NamedValue { value, .. } => render_value_inline(doc, value),
        Value::Node(id) => match doc.node_kind(*id) {
            NodeKind::Null => "null".into(),
            NodeKind::Bool => match doc.value_bytes(*id) {
                Some(b"true") => "true".into(),
                _ => "false".into(),
            },
            NodeKind::Number | NodeKind::String => doc
                .value_bytes(*id)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default(),
            NodeKind::Array | NodeKind::Object => {
                // Avoid recursing into source nodes here — their full
                // bytes can be large. Render a compact placeholder
                // that still tells the reader what they're looking at.
                let count = doc.records()[*id as usize].child_count;
                let kind = doc.node_kind(*id);
                container_preview(kind, count)
            }
        },
    }
}

/// Best-effort `NodeKind` for a synthetic value when we need to label a
/// row by its inner value's type (e.g. an unwrapped single-field
/// object). `Value::Node` doesn't appear inside synthetic output trees
/// in practice, but we fall through to `Object` for safety.
fn inner_kind(v: &Value) -> NodeKind {
    match v {
        Value::Null | Value::Group { n: None, .. } => NodeKind::Null,
        Value::Bool(_) => NodeKind::Bool,
        Value::Number(_) | Value::Group { n: Some(_), .. } => NodeKind::Number,
        Value::Str(_) => NodeKind::String,
        Value::Object(_) | Value::BucketRow(_) | Value::NamedValue { .. } => NodeKind::Object,
        Value::GroupList { .. } => NodeKind::Array,
        Value::Node(_) => NodeKind::Object,
    }
}

fn synthetic_result(s: &str, kind: NodeKind) -> QueryResult {
    QueryResult {
        node_id: NULL_NODE,
        kind: kind_to_u8(kind),
        path: format!("(synthetic) {}", truncate(s, 64)),
        preview: truncate(s, 80),
        full_text: s.to_string(),
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
    // step back to a UTF-8 boundary
    let mut cut = byte_limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(cut + 3);
    out.push_str(&s[..cut]);
    out.push('…');
    out
}

pub(super) fn kind_to_u8(kind: NodeKind) -> u8 {
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
    out
}
