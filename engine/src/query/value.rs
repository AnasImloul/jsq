//! Value model for the evaluator. A value flowing through a query is
//! either a reference to a node in the engine's index, or a synthetic
//! scalar produced by a literal or a builtin.
//!
//! For the supported Tier 2 features in this iteration (comparison,
//! select, length, type), synthetic values are scalars only — there's
//! no need yet for synthetic arrays/objects (those arrive with `keys`,
//! `values`, etc.).

#[derive(Clone, Debug)]
pub enum Value {
    Node(u32),
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    /// Synthetic labeled aggregate emitted by `group_*` reducers.
    /// Renders with `key` as the result-row path and `n` as the value
    /// preview. `None` means the reducer saw no rows / no values —
    /// distinct from `Some(0.0)` (the sum was actually zero).
    Group { key: String, n: Option<f64> },
    /// Synthetic group emitted by `by KEY` (no reducer). Carries the
    /// list of node IDs that fell into the bucket, so the UI can show
    /// the count and click through to the first member.
    GroupList { key: String, members: Vec<u32> },
    /// Synthetic object emitted by surface `select { name: expr, ... }`
    /// projections. Field order is preserved as written. A missing
    /// projection (the value expression emitted nothing) records
    /// `Value::Null` for that field.
    Object(Vec<(String, Value)>),
    /// One row of an aggregate-with-by result. Same payload shape as
    /// `Object` — the first field is the group key, the rest are the
    /// reductions — but `make_result` surfaces the key as the row's
    /// path and renders only the reductions in the value column.
    /// Field-by-name lookups (used by `order by` and `select { }`
    /// after the aggregate) still work because the field list is
    /// identical to a plain Object.
    BucketRow(Vec<(String, Value)>),
    /// Named non-scalar output from an `aggregate { ... }` block with
    /// no `by` clause. `name` is the user-written item name; `value`
    /// is the (object, group-list, …) inner result. Kept distinct from
    /// `Object(vec![(name, value)])` so single-field `select { ... }`
    /// projections — which legitimately produce a one-key Object —
    /// don't get conflated with this naming wrapper. `make_result`
    /// surfaces `name` as the row's path and the inner value's JSON
    /// in the value column.
    NamedValue { name: String, value: Box<Value> },
    /// Synthetic JSON array produced by surface array construction
    /// `[e1, e2, ...]`. Holds the materialised element values in order.
    Array(Vec<Value>),
}

impl Value {
    /// Truthiness per jq: only `null` and `false` are falsy.
    pub fn is_truthy(&self, doc: &crate::document::Document) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(_) | Value::Str(_) => true,
            // An empty-result group (n=None) is falsy, mirroring null;
            // a populated group is truthy regardless of magnitude.
            Value::Group { n, .. } => n.is_some(),
            Value::GroupList { members, .. } => !members.is_empty(),
            Value::Object(fields) | Value::BucketRow(fields) => !fields.is_empty(),
            // jq treats every array — including `[]` — as truthy.
            Value::Array(_) => true,
            Value::NamedValue { value, .. } => value.is_truthy(doc),
            Value::Node(id) => match doc.node_kind(*id) {
                crate::document::NodeKind::Null => false,
                crate::document::NodeKind::Bool => {
                    matches!(doc.value_bytes(*id), Some(b"true"))
                }
                _ => true,
            },
        }
    }
}

/// Normalized scalar view for equality and ordering.
#[derive(Clone, Debug)]
pub enum Scalar {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    /// Container or otherwise non-scalar — never equal to anything else,
    /// never ordered. Carries a tag so two distinct containers compare
    /// unequal as well.
    Container(u32),
}

impl Scalar {
    pub fn from_value(doc: &crate::document::Document, v: &Value) -> Self {
        use crate::document::NodeKind;
        match v {
            Value::Null => Scalar::Null,
            Value::Bool(b) => Scalar::Bool(*b),
            Value::Number(n) => Scalar::Number(*n),
            Value::Str(s) => Scalar::Str(s.clone()),
            // Treat a labeled aggregate as the underlying number when
            // it falls through into a comparison or sort. An empty
            // group (n=None) compares/orders as null.
            Value::Group { n: Some(v), .. } => Scalar::Number(*v),
            Value::Group { n: None, .. } => Scalar::Null,
            // GroupLists don't have a single scalar — used as containers.
            Value::GroupList { .. } => Scalar::Container(0),
            // Synthetic objects flow through comparison/sort as opaque
            // containers — equality is by identity (always unequal here
            // since we use 0 for all). Refine if/when projections feed
            // into comparisons in a meaningful way.
            Value::Object(_) | Value::BucketRow(_) | Value::NamedValue { .. } | Value::Array(_) => Scalar::Container(0),
            Value::Node(id) => match doc.node_kind(*id) {
                NodeKind::Null => Scalar::Null,
                NodeKind::Bool => Scalar::Bool(matches!(doc.value_bytes(*id), Some(b"true"))),
                NodeKind::Number => match doc.value_bytes(*id).and_then(|b| std::str::from_utf8(b).ok()) {
                    Some(s) => match s.parse::<f64>() {
                        Ok(n) => Scalar::Number(n),
                        Err(_) => Scalar::Container(*id),
                    },
                    None => Scalar::Container(*id),
                },
                NodeKind::String => match doc.value_bytes(*id) {
                    Some(b) => Scalar::Str(decode_json_string(b)),
                    None => Scalar::Container(*id),
                },
                NodeKind::Array | NodeKind::Object => Scalar::Container(*id),
            },
        }
    }

    pub fn equal(&self, other: &Scalar) -> bool {
        match (self, other) {
            (Scalar::Null, Scalar::Null) => true,
            (Scalar::Bool(a), Scalar::Bool(b)) => a == b,
            (Scalar::Number(a), Scalar::Number(b)) => a == b, // NaN != NaN, fine for jq
            (Scalar::Str(a), Scalar::Str(b)) => a == b,
            (Scalar::Container(a), Scalar::Container(b)) => a == b,
            _ => false,
        }
    }

    pub fn compare(&self, other: &Scalar) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering;
        match (self, other) {
            (Scalar::Null, Scalar::Null) => Some(Ordering::Equal),
            (Scalar::Bool(a), Scalar::Bool(b)) => Some(a.cmp(b)),
            (Scalar::Number(a), Scalar::Number(b)) => a.partial_cmp(b),
            (Scalar::Str(a), Scalar::Str(b)) => Some(a.cmp(b)),
            // Cross-type ordering: not supported in this iteration.
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Scalar::Null => "null",
            Scalar::Bool(_) => "boolean",
            Scalar::Number(_) => "number",
            Scalar::Str(_) => "string",
            Scalar::Container(_) => "container",
        }
    }
}

/// Decodes a JSON-quoted UTF-8 byte slice (e.g. b"\"hello\\n\"") into the
/// underlying string. Best-effort: malformed input yields a lossy result.
pub fn decode_json_string(bytes: &[u8]) -> String {
    if bytes.len() < 2 || bytes[0] != b'"' {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let inner = &bytes[1..bytes.len().saturating_sub(1)];
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b == b'\\' && i + 1 < inner.len() {
            match inner[i + 1] {
                b'"' => { out.push('"'); i += 2; }
                b'\\' => { out.push('\\'); i += 2; }
                b'/' => { out.push('/'); i += 2; }
                b'b' => { out.push('\u{08}'); i += 2; }
                b'f' => { out.push('\u{0C}'); i += 2; }
                b'n' => { out.push('\n'); i += 2; }
                b'r' => { out.push('\r'); i += 2; }
                b't' => { out.push('\t'); i += 2; }
                b'u' => {
                    let high = parse_hex4(&inner[i + 2..]);
                    if let Some(h) = high {
                        if (0xD800..=0xDBFF).contains(&h)
                            && i + 12 <= inner.len()
                            && inner[i + 6] == b'\\'
                            && inner[i + 7] == b'u'
                        {
                            if let Some(low) = parse_hex4(&inner[i + 8..]) {
                                let code = 0x10000 + ((h - 0xD800) << 10) + (low - 0xDC00);
                                if let Some(ch) = char::from_u32(code) {
                                    out.push(ch);
                                }
                                i += 12;
                                continue;
                            }
                        }
                        if let Some(ch) = char::from_u32(h) {
                            out.push(ch);
                        }
                        i += 6;
                    } else {
                        out.push('\u{FFFD}');
                        i += 2;
                    }
                }
                _ => { out.push('\u{FFFD}'); i += 2; }
            }
        } else {
            // Append one UTF-8 sequence
            let len = utf8_seq_len(b);
            let end = (i + len).min(inner.len());
            out.push_str(&String::from_utf8_lossy(&inner[i..end]));
            i = end;
        }
    }
    out
}

fn parse_hex4(buf: &[u8]) -> Option<u32> {
    if buf.len() < 4 {
        return None;
    }
    let mut code = 0u32;
    for &b in &buf[..4] {
        let v = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return None,
        };
        code = code * 16 + v;
    }
    Some(code)
}

fn utf8_seq_len(first: u8) -> usize {
    if first < 0x80 { 1 }
    else if first < 0xC0 { 1 } // continuation byte; treat as single
    else if first < 0xE0 { 2 }
    else if first < 0xF0 { 3 }
    else { 4 }
}
