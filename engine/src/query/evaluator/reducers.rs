//! Per-bucket reducer state and the `EXPR by KEY` postfix runners.
//!
//! Every reducer (`sum`, `min`, `max`, `avg`, `count`) feeds a single
//! [`ReducerState`] — one struct holds enough to satisfy any of the
//! five ops, so the hot inner loop has no per-op branching beyond
//! `consume`. [`run_by`] dispatches to the reducer/collect modes
//! depending on the shape of `EXPR`.

use std::collections::HashMap;

use crate::document::{Document, NodeKind};

use super::super::ast::{Ast, ReducerOp};
use super::super::index::ScalarKey;
use super::super::value::Value;
use super::fingerprint::first_scalar_key;
use super::walk::walk;

#[derive(Default)]
pub(super) struct ReducerState {
    pub(super) sum: f64,
    pub(super) count: u64,
    /// Used by Min/Max only.
    pub(super) extreme: Option<f64>,
}

impl ReducerState {
    pub(super) fn consume(&mut self, doc: &Document, op: ReducerOp, v: &Value) {
        if matches!(op, ReducerOp::Count) {
            self.count += 1;
            return;
        }
        if let Some(n) = to_number(doc, v) {
            self.sum += n;
            self.count += 1;
            self.extreme = Some(match (op, self.extreme) {
                (ReducerOp::Min, Some(cur)) if cur <= n => cur,
                (ReducerOp::Max, Some(cur)) if cur >= n => cur,
                _ => n,
            });
        }
    }

    /// Finalises the reducer, distinguishing "no inputs" from "inputs
    /// summed to zero". Returns `None` for Sum/Min/Max/Avg when the
    /// reducer never consumed a value; Count always returns
    /// `Some(count)` since the empty count is well-defined as 0.
    pub(super) fn finalize_optional(&self, op: ReducerOp) -> Option<f64> {
        match op {
            ReducerOp::Sum => if self.count == 0 { None } else { Some(self.sum) },
            ReducerOp::Count => Some(self.count as f64),
            ReducerOp::Min => self.extreme,
            ReducerOp::Max => self.extreme,
            ReducerOp::Avg => {
                if self.count == 0 { None } else { Some(self.sum / self.count as f64) }
            }
        }
    }
}

/// Coerces a query value to a finite f64 for the numeric reducers.
/// Non-numeric values become `None` so the reducer silently skips them
/// rather than poisoning the running aggregate with NaN.
pub(super) fn to_number(doc: &Document, v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.is_finite().then_some(*n),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Group { n, .. } => n.and_then(|v| v.is_finite().then_some(v)),
        Value::GroupList { members, .. } => Some(members.len() as f64),
        Value::Null | Value::Str(_) | Value::Object(_) | Value::BucketRow(_) | Value::NamedValue { .. } => None,
        Value::Node(id) => {
            if doc.node_kind(*id) != NodeKind::Number {
                return None;
            }
            let bytes = doc.value_bytes(*id)?;
            let s = std::str::from_utf8(bytes).ok()?;
            let n: f64 = s.parse().ok()?;
            n.is_finite().then_some(n)
        }
    }
}

/// Decomposes `expr` into `(reducer_op, optional_value_expr)`. Accepts
/// either a bare reducer (`count`) or a pipeline ending in a reducer
/// (`.balance | sum`, `.x | .y | sum`). Returns `None` if `expr` isn't
/// shaped like a reducer at all — `by` then degrades gracefully.
pub(super) fn detect_reducer<'a>(expr: &'a Ast) -> Option<(ReducerOp, Option<&'a Ast>)> {
    let op_of = |a: &Ast| -> Option<ReducerOp> {
        match a {
            Ast::Sum => Some(ReducerOp::Sum),
            Ast::Min => Some(ReducerOp::Min),
            Ast::Max => Some(ReducerOp::Max),
            Ast::Avg => Some(ReducerOp::Avg),
            Ast::Count => Some(ReducerOp::Count),
            _ => None,
        }
    };
    if let Some(op) = op_of(expr) {
        return Some((op, None));
    }
    if let Ast::Pipe(l, r) = expr {
        if let Some(op) = op_of(r) {
            return Some((op, Some(l.as_ref())));
        }
    }
    None
}

/// Drives the `EXPR by KEY` postfix. Two modes:
///
/// 1. *Reducer mode* — `EXPR` is a recognised reducer (`sum`, `count`,
///    `min`, `max`, `avg`) optionally piped from a value-extractor:
///    walks LHS, partitions by KEY, applies the reducer per bucket,
///    emits one `Value::Group { key, n }` per bucket.
/// 2. *Collect mode* — `EXPR` is anything else (typically `Identity`
///    for the bare `by KEY` form, or a chain like `.name` for
///    `.name by KEY`): walks LHS, partitions by KEY, accumulates each
///    row's `EXPR` outputs into a per-bucket list of node IDs, emits
///    one `Value::GroupList { key, members }` per bucket.
pub(super) fn run_by(
    doc: &Document,
    l: &Ast,
    expr: &Ast,
    key: &Ast,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    if let Some((op, value_expr)) = detect_reducer(expr) {
        return run_by_reducer(doc, l, key, input, sink, op, value_expr);
    }
    run_by_collect(doc, l, expr, key, input, sink)
}

fn run_by_reducer(
    doc: &Document,
    l: &Ast,
    key: &Ast,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
    op: ReducerOp,
    value_expr: Option<&Ast>,
) -> bool {
    // Bucket map keyed by `ScalarKey` rather than rendered String,
    // so numeric and bool keys skip the per-row String allocation
    // that string-rendering would force.
    let mut buckets: HashMap<ScalarKey, ReducerState> = HashMap::new();

    walk(doc, l, input, &mut |row| {
        let bucket_key = match first_scalar_key(doc, key, &row) {
            Some(k) => k,
            None => return true,
        };
        let state = buckets.entry(bucket_key).or_default();
        match value_expr {
            Some(ve) => {
                walk(doc, ve, row, &mut |v| {
                    state.consume(doc, op, &v);
                    true
                });
            }
            None => {
                state.consume(doc, op, &row);
            }
        }
        true
    });

    // Sort buckets by their `ScalarKey` directly. Comparing keys in
    // their typed form (integer / float / lexicographic) avoids the
    // per-bucket String render that would otherwise be needed up-front;
    // rendering is deferred to sink time, where each bucket is rendered
    // once just before emission.
    let mut entries: Vec<(ScalarKey, ReducerState)> = buckets.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for (sk, state) in entries {
        let n = state.finalize_optional(op);
        if !sink(Value::Group { key: sk.render_as_string(), n }) {
            return false;
        }
    }
    true
}

/// Collect mode: emits `Value::GroupList { key, members }` per bucket,
/// where `members` is the list of node IDs that landed there. Skips
/// non-Node values from `expr`'s output — for the typical `by KEY` case
/// where `expr` is `Identity`, every row is a Node from the upstream.
fn run_by_collect(
    doc: &Document,
    l: &Ast,
    expr: &Ast,
    key: &Ast,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let mut buckets: HashMap<ScalarKey, Vec<u32>> = HashMap::new();

    walk(doc, l, input, &mut |row| {
        let bucket_key = match first_scalar_key(doc, key, &row) {
            Some(k) => k,
            None => return true,
        };
        let entry = buckets.entry(bucket_key).or_default();
        walk(doc, expr, row, &mut |v| {
            if let Value::Node(id) = v {
                entry.push(id);
            }
            true
        });
        true
    });

    let mut entries: Vec<(ScalarKey, Vec<u32>)> = buckets.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for (sk, members) in entries {
        if !sink(Value::GroupList { key: sk.render_as_string(), members }) {
            return false;
        }
    }
    true
}

/// Streaming reducer for the bare `LHS | sum` shape (and friends).
/// Walks LHS once, accumulates into a `ReducerState`, emits one final
/// value. Used inline by the `Pipe(_, Sum/Min/Max/Avg/Count)` arms in
/// `walk` so the generic shape can stay in one place.
pub(super) fn run_streaming_reducer(
    doc: &Document,
    l: &Ast,
    op: ReducerOp,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let mut state = ReducerState::default();
    walk(doc, l, input, &mut |v| {
        state.consume(doc, op, &v);
        true
    });
    match state.finalize_optional(op) {
        Some(n) => sink(Value::Number(n)),
        None => sink(Value::Null),
    }
}
