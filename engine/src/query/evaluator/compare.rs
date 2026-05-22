//! Comparison operators for `Ast::Compare` and `Ast::FieldSetEquals`.
//!
//! [`compare_values`] dispatches on `CompareOp` and is the canonical
//! implementation; [`fast_equal`] is a specialisation that skips the
//! `Scalar::from_value` round-trip on `(Node, Literal)` pairs since
//! `FieldSetEquals` runs hundreds of comparisons per row. `glob_match`
//! and `string_op` are private helpers used by both.

use std::cmp::Ordering;

use crate::document::{Document, NodeKind};

use super::super::ast::CompareOp;
use super::super::value::{Scalar, Value};

pub(super) fn compare_values(doc: &Document, l: &Value, op: CompareOp, r: &Value) -> bool {
    let ls = Scalar::from_value(doc, l);
    let rs = Scalar::from_value(doc, r);
    match op {
        CompareOp::Eq => ls.equal(&rs),
        CompareOp::Ne => !ls.equal(&rs),
        CompareOp::Lt => matches!(ls.compare(&rs), Some(Ordering::Less)),
        CompareOp::Le => matches!(ls.compare(&rs), Some(Ordering::Less | Ordering::Equal)),
        CompareOp::Gt => matches!(ls.compare(&rs), Some(Ordering::Greater)),
        CompareOp::Ge => matches!(ls.compare(&rs), Some(Ordering::Greater | Ordering::Equal)),
        CompareOp::Contains => string_op(&ls, &rs, |a, b| a.contains(b)),
        CompareOp::StartsWith => string_op(&ls, &rs, |a, b| a.starts_with(b)),
        CompareOp::EndsWith => string_op(&ls, &rs, |a, b| a.ends_with(b)),
        CompareOp::Matches => string_op(&ls, &rs, |a, b| glob_match(a, b)),
    }
}

/// Equality test specialised for the common `(Node, Literal)` shape:
/// compares the node's source bytes directly against the literal's
/// bytes, skipping the `Scalar::from_value` round-trip and the
/// per-call `decode_json_string` allocation it would force. Falls back
/// to full `Scalar` comparison for anything else.
pub(super) fn fast_equal(doc: &Document, a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Node(id), Value::Str(s)) | (Value::Str(s), Value::Node(id)) => {
            if doc.node_kind(*id) != NodeKind::String {
                return false;
            }
            let bytes = match doc.value_bytes(*id) {
                Some(b) => b,
                None => return false,
            };
            json_string_bytes_equal(bytes, s.as_bytes())
        }
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => {
            let sa = Scalar::from_value(doc, a);
            let sb = Scalar::from_value(doc, b);
            sa.equal(&sb)
        }
    }
}

/// Lifts a `&str → &str → bool` predicate over two `Scalar`s. Non-string
/// inputs (numbers, nulls, containers, …) drop the comparison to false —
/// matching jq's instinct that a string predicate against a non-string
/// is "no, it doesn't match" rather than an error.
fn string_op(l: &Scalar, r: &Scalar, f: impl Fn(&str, &str) -> bool) -> bool {
    match (l, r) {
        (Scalar::Str(a), Scalar::Str(b)) => f(a, b),
        _ => false,
    }
}

/// Compares the inner of a JSON-quoted byte slice (`"..."`) against a
/// raw target. The fast path — and the typical one for object-key /
/// short-string compares — does a single `slice == slice` when the
/// inner contains no escape sequences. The slow path delegates to the
/// full decoder so escapes match correctly.
fn json_string_bytes_equal(json_bytes: &[u8], target: &[u8]) -> bool {
    if json_bytes.len() < 2
        || json_bytes[0] != b'"'
        || json_bytes[json_bytes.len() - 1] != b'"'
    {
        return false;
    }
    let inner = &json_bytes[1..json_bytes.len() - 1];
    if !inner.contains(&b'\\') {
        return inner == target;
    }
    let decoded = super::super::value::decode_json_string(json_bytes);
    decoded.as_bytes() == target
}

/// Glob match supporting `*` (any run, including empty) and `?` (exactly
/// one character). All other characters are literal, including `.` and
/// regex-style metas — by design, this is *not* a regex. Iterative
/// implementation with a backtrack pointer; O(n*m) worst case, fine for
/// the short patterns we expect.
fn glob_match(s: &str, pat: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    let mut si = 0usize;
    let mut pi = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_si = 0usize;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            si += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_si = si;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_si += 1;
            si = star_si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}
