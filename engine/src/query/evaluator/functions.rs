//! Strict scalar function dispatch — the runtime side of `Ast::Call`.
//!
//! Each argument arrives already collapsed to its first emission (or
//! `None` when the argument expression emitted nothing); the walk arm
//! does that evaluation so this module stays free of recursion. Arity is
//! validated when lowering builds the `Ast::Call`, so an unexpected count
//! here degrades to `Value::Null` rather than panicking.

use crate::document::{Document, NodeKind};

use super::super::grammar::kw;
use super::super::value::{decode_json_string, Value};
use super::walk::to_number;

pub(super) fn call(doc: &Document, name: &str, args: &[Option<Value>]) -> Value {
    match name {
        kw::ROUND => round(doc, args),
        kw::LENGTH => length(doc, arg(args, 0)),
        kw::LOWER => map_str(doc, arg(args, 0), |s| s.to_lowercase()),
        kw::UPPER => map_str(doc, arg(args, 0), |s| s.to_uppercase()),
        kw::ABS => map_num(doc, arg(args, 0), f64::abs),
        kw::FLOOR => map_num(doc, arg(args, 0), f64::floor),
        kw::CEIL => map_num(doc, arg(args, 0), f64::ceil),
        kw::SQRT => map_num(doc, arg(args, 0), f64::sqrt),
        kw::POW => map_num2(doc, arg(args, 0), arg(args, 1), f64::powf),
        kw::MOD => map_num2(doc, arg(args, 0), arg(args, 1), |a, b| {
            if b == 0.0 { f64::NAN } else { a % b }
        }),
        kw::TRIM => map_str(doc, arg(args, 0), |s| s.trim().to_string()),
        kw::SUBSTR => substr(doc, args),
        kw::REPLACE => replace(doc, args),
        _ => Value::Null,
    }
}

fn arg(args: &[Option<Value>], i: usize) -> Option<&Value> {
    args.get(i).and_then(|o| o.as_ref())
}

fn map_num(doc: &Document, v: Option<&Value>, f: impl Fn(f64) -> f64) -> Value {
    match to_number(doc, v.cloned()) {
        Some(n) => {
            let r = f(n);
            if r.is_finite() { Value::Number(r) } else { Value::Null }
        }
        None => Value::Null,
    }
}

fn map_num2(
    doc: &Document,
    a: Option<&Value>,
    b: Option<&Value>,
    f: impl Fn(f64, f64) -> f64,
) -> Value {
    match (to_number(doc, a.cloned()), to_number(doc, b.cloned())) {
        (Some(x), Some(y)) => {
            let r = f(x, y);
            if r.is_finite() { Value::Number(r) } else { Value::Null }
        }
        _ => Value::Null,
    }
}

fn as_string(doc: &Document, v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Str(s) => Some(s.clone()),
        Value::Node(id) if doc.node_kind(*id) == NodeKind::String => {
            doc.value_bytes(*id).map(decode_json_string)
        }
        _ => None,
    }
}

fn map_str(doc: &Document, v: Option<&Value>, f: impl Fn(&str) -> String) -> Value {
    match as_string(doc, v) {
        Some(s) => Value::Str(f(&s)),
        None => Value::Null,
    }
}

/// `length` — string codepoints, array/object element counts, `0` for
/// null. Plain numbers / booleans have no length here and yield null
/// (`abs` covers numeric magnitude).
fn length(doc: &Document, v: Option<&Value>) -> Value {
    let n: usize = match v {
        None | Some(Value::Null) => 0,
        Some(Value::Str(s)) => s.chars().count(),
        Some(Value::Array(items)) => items.len(),
        Some(Value::GroupList { members, .. }) => members.len(),
        Some(Value::Object(fields)) | Some(Value::BucketRow(fields)) => fields.len(),
        Some(Value::Node(id)) => match doc.node_kind(*id) {
            NodeKind::Null => 0,
            NodeKind::String => doc
                .value_bytes(*id)
                .map(|b| decode_json_string(b).chars().count())
                .unwrap_or(0),
            NodeKind::Array | NodeKind::Object => {
                doc.records()[*id as usize].child_count as usize
            }
            _ => return Value::Null,
        },
        Some(_) => return Value::Null,
    };
    Value::Number(n as f64)
}

/// `round(VALUE [, PRECISION])`. Non-numeric value or non-finite
/// precision produce null; absent precision rounds to the nearest
/// integer. Negative precision rounds to tens / hundreds / …
fn round(doc: &Document, args: &[Option<Value>]) -> Value {
    let v = to_number(doc, args.first().cloned().flatten());
    let p = match args.get(1) {
        Some(p) => to_number(doc, p.clone()),
        None => Some(0.0),
    };
    let result: Option<f64> = match (v, p) {
        (Some(x), Some(p)) if p.is_finite() => {
            // Clamp precision so 10^p stays representable.
            let clamped = (p.round() as i32).clamp(-308, 308);
            let factor = 10f64.powi(clamped);
            if factor.is_finite() && factor != 0.0 {
                Some((x * factor).round() / factor)
            } else {
                None
            }
        }
        _ => None,
    };
    match result {
        Some(n) if n.is_finite() => Value::Number(n),
        _ => Value::Null,
    }
}

/// `substr(S, START, LEN)` — codepoint-based slice. `START` is clamped
/// to `[0, len]`; `LEN` is clamped so the slice never runs past the end.
/// A non-string `S` or non-numeric `START`/`LEN` yields null.
fn substr(doc: &Document, args: &[Option<Value>]) -> Value {
    let s = match as_string(doc, arg(args, 0)) {
        Some(s) => s,
        None => return Value::Null,
    };
    let (start, len) = match (
        to_number(doc, args.get(1).cloned().flatten()),
        to_number(doc, args.get(2).cloned().flatten()),
    ) {
        (Some(a), Some(b)) if a.is_finite() && b.is_finite() => (a, b),
        _ => return Value::Null,
    };
    let chars: Vec<char> = s.chars().collect();
    let total = chars.len();
    let begin = (start.trunc() as i64).clamp(0, total as i64) as usize;
    let count = len.trunc().max(0.0) as usize;
    let end = begin.saturating_add(count).min(total);
    Value::Str(chars[begin..end].iter().collect())
}

/// `replace(S, FROM, TO)` — literal replace-all. Empty `FROM` returns
/// `S` unchanged (matches Rust's `str::replace`, which would otherwise
/// splice `TO` between every codepoint). Non-string operands yield null.
fn replace(doc: &Document, args: &[Option<Value>]) -> Value {
    let (s, from, to) = match (
        as_string(doc, arg(args, 0)),
        as_string(doc, arg(args, 1)),
        as_string(doc, arg(args, 2)),
    ) {
        (Some(s), Some(from), Some(to)) => (s, from, to),
        _ => return Value::Null,
    };
    if from.is_empty() {
        return Value::Str(s);
    }
    Value::Str(s.replace(&from, &to))
}
