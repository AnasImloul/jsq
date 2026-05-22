//! `aggregate { name = OP EXPR where P, ... } by KEY` — the multi-aggregate
//! block. Each named reduction owns a `ReducerState` slot inside the
//! shared `AggregateBucket`; one bucket exists per group key (or one
//! synthetic bucket when no `by` is present).

use std::collections::HashMap;

use crate::document::Document;

use super::super::ast::{AggGroup, AggOutputNode, AggReduction, Ast, ReducerOp};
use super::super::index::ScalarKey;
use super::super::value::Value;
use super::reducers::ReducerState;
use super::walk::{first_emission, walk};
use super::{pop_reducer_slots, push_reducer_slots};

pub(super) struct AggregateBucket {
    key_value: Option<Value>,
    states: Vec<ReducerState>,
}

impl AggregateBucket {
    fn new(reductions: &[AggReduction], key_value: Option<Value>) -> Self {
        Self {
            key_value,
            states: (0..reductions.len()).map(|_| ReducerState::default()).collect(),
        }
    }
}

pub(super) fn run_aggregate_block(
    doc: &Document,
    upstream: &Ast,
    group: Option<&AggGroup>,
    reductions: &[AggReduction],
    outputs: &[(String, AggOutputNode)],
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let mut single_bucket: Option<AggregateBucket> = group
        .is_none()
        .then(|| AggregateBucket::new(reductions, None));
    let mut buckets: HashMap<ScalarKey, AggregateBucket> = HashMap::new();

    walk(doc, upstream, input, &mut |row| {
        process_aggregate_row(
            doc,
            group,
            reductions,
            row,
            &mut single_bucket,
            &mut buckets,
        );
        true
    });

    if let Some(bucket) = single_bucket {
        return emit_no_by(doc, reductions, outputs, bucket, sink);
    }
    emit_bucketed(doc, group, reductions, outputs, buckets, sink)
}

/// Per-row body for `run_aggregate_block`. Updates `single_bucket`
/// (no-`by` form) or `buckets` (bucketed form) in place.
fn process_aggregate_row(
    doc: &Document,
    group: Option<&AggGroup>,
    reductions: &[AggReduction],
    row: Value,
    single_bucket: &mut Option<AggregateBucket>,
    buckets: &mut HashMap<ScalarKey, AggregateBucket>,
) {
    let bucket = match (group, single_bucket.as_mut()) {
        (Some(g), _) => {
            let mut key_sk: Option<ScalarKey> = None;
            let mut key_val: Option<Value> = None;
            walk(doc, &g.key, row.clone(), &mut |v| {
                if key_sk.is_none() {
                    if let Some(sk) = ScalarKey::from_value(doc, &v) {
                        key_sk = Some(sk);
                        key_val = Some(v);
                    }
                }
                false
            });
            let (key_sk, key_val) = match (key_sk, key_val) {
                (Some(s), Some(v)) => (s, v),
                _ => return, // missing / non-scalar key — drop row
            };
            buckets
                .entry(key_sk)
                .or_insert_with(|| AggregateBucket::new(reductions, Some(key_val)))
        }
        (None, Some(b)) => b,
        (None, None) => return, // shouldn't happen in well-formed callers
    };

    for (i, red) in reductions.iter().enumerate() {
        if let Some(pred) = &red.where_pred {
            let mut keep = false;
            walk(doc, pred, row.clone(), &mut |v| {
                if v.is_truthy(doc) {
                    keep = true;
                }
                true
            });
            if !keep {
                continue;
            }
        }
        match (&red.value, red.op) {
            (None, ReducerOp::Count) => {
                bucket.states[i].count += 1;
            }
            (Some(v_expr), ReducerOp::Count) => {
                walk(doc, v_expr, row.clone(), &mut |v| {
                    if !matches!(v, Value::Null) {
                        bucket.states[i].count += 1;
                    }
                    true
                });
            }
            (Some(v_expr), op) => {
                walk(doc, v_expr, row.clone(), &mut |v| {
                    bucket.states[i].consume(doc, op, &v);
                    true
                });
            }
            (None, _) => {
                bucket.states[i].consume(doc, red.op, &row);
            }
        }
    }
}

/// No-`by` form: every output lands on its own row labelled with its
/// name. Reads better in the table than a single multi-field synthetic
/// object — and since there's no group key, the row's "path" column has
/// somewhere meaningful to point.
fn emit_no_by(
    doc: &Document,
    reductions: &[AggReduction],
    outputs: &[(String, AggOutputNode)],
    bucket: AggregateBucket,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let slots = finalize_slots(reductions, &bucket);
    push_reducer_slots(slots);
    let result = (|| {
        for (name, node) in outputs {
            let resolved = evaluate_output_node(doc, node);
            let final_value = group_value(name, resolved);
            if !sink(final_value) {
                return false;
            }
        }
        true
    })();
    pop_reducer_slots();
    result
}

/// Picks a result-row encoding for a no-by output value, keeping the
/// `Value::Group` shape for numbers / nulls (so the row renders as
/// `name → value`) and falling back to a single-field object for other
/// shapes (string default, etc.).
fn group_value(name: &str, v: Value) -> Value {
    match v {
        Value::Number(n) => Value::Group { key: name.to_string(), n: Some(n) },
        Value::Null => Value::Group { key: name.to_string(), n: None },
        other => Value::NamedValue {
            name: name.to_string(),
            value: Box::new(other),
        },
    }
}

/// Bucketed form: emits one `Value::BucketRow` per group, with the
/// group key as the first field and the outputs as the remaining
/// fields. `BucketRow` tells the renderer to surface the key in the
/// path column and put just the outputs in the value column.
fn emit_bucketed(
    doc: &Document,
    group: Option<&AggGroup>,
    reductions: &[AggReduction],
    outputs: &[(String, AggOutputNode)],
    buckets: HashMap<ScalarKey, AggregateBucket>,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let key_name = group.map(|g| g.name.as_str()).unwrap_or("key");
    // Sort by typed `ScalarKey`; see the same pattern in
    // `reducers::run_by_reducer` for why this avoids a per-bucket
    // render up-front.
    let mut entries: Vec<(ScalarKey, AggregateBucket)> = buckets.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for (_sk, bucket) in entries {
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(outputs.len() + 1);
        if let Some(kv) = bucket.key_value.clone() {
            fields.push((key_name.to_string(), kv));
        }
        let slots = finalize_slots(reductions, &bucket);
        push_reducer_slots(slots);
        for (name, node) in outputs {
            let resolved = evaluate_output_node(doc, node);
            fields.push((name.clone(), resolved));
        }
        pop_reducer_slots();
        if !sink(Value::BucketRow(fields)) {
            return false;
        }
    }
    true
}

/// Pre-finalises every reduction's state into an `Option<f64>` slot.
/// `None` means the reducer saw no inputs; the surface lowerer never
/// generates a non-numeric reducer, so the slot type is always
/// numeric.
fn finalize_slots(reductions: &[AggReduction], bucket: &AggregateBucket) -> Vec<Option<f64>> {
    reductions
        .iter()
        .enumerate()
        .map(|(i, red)| bucket.states[i].finalize_optional(red.op))
        .collect()
}

/// Evaluates one output node against the current bucket's slot frame
/// (must already be pushed). `Leaf` nodes apply the item-level `??`
/// default when the expression evaluates to `Value::Null` — the catch-
/// all for "every reducer saw nothing, OR a divide-by-zero, OR a non-
/// numeric operand somewhere". `Object` nodes recurse and assemble a
/// nested `Value::Object`.
fn evaluate_output_node(doc: &Document, node: &AggOutputNode) -> Value {
    match node {
        AggOutputNode::Leaf { expr, default } => {
            let primary = first_emission(doc, expr, Value::Null).unwrap_or(Value::Null);
            if !matches!(primary, Value::Null) {
                return primary;
            }
            if let Some(default_ast) = default {
                return first_emission(doc, default_ast, Value::Null).unwrap_or(Value::Null);
            }
            Value::Null
        }
        AggOutputNode::Object(fields) => {
            let resolved: Vec<(String, Value)> = fields
                .iter()
                .map(|(k, child)| (k.clone(), evaluate_output_node(doc, child)))
                .collect();
            Value::Object(resolved)
        }
    }
}
