//! Push-based AST walker. `walk` is the single dispatch point: it
//! pattern-matches on `Ast` and feeds each output value to the supplied
//! sink. Stateful operators (reducers, sort, distinct, aggregate-block)
//! delegate to dedicated sibling modules — see [`super::reducers`],
//! [`super::aggregate`], [`super::sort`], and [`super::fingerprint`].

use crate::document::{Document, NodeKind};

use super::super::ast::{Ast, BinaryOp, ReducerOp};
use super::super::index::ScalarKey;
use super::super::value::Value;
use super::aggregate::run_aggregate_block;
use super::compare::{compare_values, fast_equal};
use super::fingerprint::{value_to_group_key, write_fingerprint};
use super::reducers::{run_by, run_streaming_reducer};
use super::scan::{
    descend_emit, descend_field_emit, scan_array_index, scan_field_set_equals, scan_iterate,
    scan_iterate_field, scan_object_field,
};
use super::sort::run_sort_by;
use super::{
    binding_get, binding_set, bump_lookup_calls, bump_scanned_bytes, bump_scanned_rows,
    lookup_resolved, outer_limit,
    reducer_slot_value, set_eval_error, EvalError, ResolvedIndex,
};

/// Public entry point used by the index-builder module to drive a walk
/// against a specific input. Mirrors `walk` exactly; renamed for clarity
/// at the crate boundary.
pub fn walk_eval(
    doc: &Document,
    ast: &Ast,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    walk(doc, ast, input, sink)
}

/// Returns false if the sink asked us to stop (limit reached).
pub(super) fn walk(
    doc: &Document,
    ast: &Ast,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    match ast {
        Ast::Identity => sink(input),

        Ast::LitNumber(n) => sink(Value::Number(*n)),
        Ast::LitString(s) => sink(Value::Str(s.clone())),
        Ast::LitBool(b) => sink(Value::Bool(*b)),
        Ast::LitNull => sink(Value::Null),

        Ast::Field(name) => {
            // Synthetic-object lookup. Lets `order by weight` reach into
            // a row produced by `select { weight: ... }` or by an
            // aggregate-with-by reduction. `BucketRow` shares the
            // payload shape with `Object`, so the same scan handles
            // both.
            if let Value::Object(fields) | Value::BucketRow(fields) = &input {
                for (k, v) in fields {
                    if k == name {
                        return sink(v.clone());
                    }
                }
                return true;
            }
            if let Value::Node(id) = input {
                if doc.node_kind(id) == NodeKind::Object {
                    if let Some(v) = scan_object_field(doc, id, name.as_bytes()) {
                        return sink(v);
                    }
                }
            }
            true
        }

        Ast::Index(i) => {
            if let Value::Node(id) = input {
                if doc.node_kind(id) == NodeKind::Array {
                    let count = doc.record(id).map(|r| r.child_count as i64).unwrap_or(0);
                    let real = if *i < 0 { count + i } else { *i };
                    if real >= 0 && real < count {
                        if let Some(v) = scan_array_index(doc, id, real as usize) {
                            return sink(v);
                        }
                    }
                }
            }
            true
        }

        Ast::Iterate => {
            if let Value::Node(id) = input {
                let kind = doc.node_kind(id);
                if matches!(kind, NodeKind::Array | NodeKind::Object) {
                    return scan_iterate(doc, id, sink);
                }
            }
            true
        }

        // Recursive descent: emit the input itself, then every
        // descendant in document order. Same shape as jq's `..`.
        Ast::Descend => {
            if let Value::Node(id) = input {
                if !descend_emit(doc, id, sink) {
                    return false;
                }
            } else if !sink(input) {
                return false;
            }
            true
        }

        // Fused `Descend | Field(name)`: walks the subtree once and
        // emits only those nodes whose own object key equals `name`.
        // O(N) over the subtree, with no temporary materialisation of
        // intermediate descendants.
        Ast::DescendField(name) => {
            if let Value::Node(id) = input {
                if !descend_field_emit(doc, id, name.as_bytes(), sink) {
                    return false;
                }
            }
            true
        }

        // Fused `Iterate | Field(name)`: walks the input container's
        // children once and emits each child object's `name` member,
        // skipping children that aren't objects or that lack the key.
        // The fused scanner skips the per-row `walk` dispatch + record
        // re-lookup and carries a slot-position hint across iterations
        // so cube-shaped data (where every sibling has the same key
        // order) avoids re-comparing every preceding key per row.
        Ast::IterateField(name) => {
            if let Value::Node(id) = input {
                let kind = doc.node_kind(id);
                if matches!(kind, NodeKind::Array | NodeKind::Object) {
                    return scan_iterate_field(doc, id, name.as_bytes(), sink);
                }
            }
            true
        }

        Ast::Pipe(l, r) => {
            // Stateful right-hand-sides: reducers, sort, limit,
            // distinct, multi-aggregate. Each runs the LHS once,
            // accumulating state, then emits its result(s) downstream.
            // State is local to this pipe so multiple reducers in one
            // query don't share counters.
            match r.as_ref() {
                Ast::Sum => return run_streaming_reducer(doc, l, ReducerOp::Sum, input, sink),
                Ast::Min => return run_streaming_reducer(doc, l, ReducerOp::Min, input, sink),
                Ast::Max => return run_streaming_reducer(doc, l, ReducerOp::Max, input, sink),
                Ast::Avg => return run_streaming_reducer(doc, l, ReducerOp::Avg, input, sink),
                Ast::Count => return run_streaming_reducer(doc, l, ReducerOp::Count, input, sink),
                Ast::By(expr, key) => return run_by(doc, l, expr, key, input, sink),
                Ast::SortBy(keys) => {
                    let k = effective_topk(None);
                    return run_sort_by(doc, l, keys, k, input, sink);
                }
                Ast::Limit(n) => {
                    // Fused `SortBy | Limit`: bound the heap to the
                    // explicit limit (or the outer cap, whichever is
                    // smaller) instead of buffering every row.
                    if let Ast::Pipe(upstream, mid) = l.as_ref() {
                        if let Ast::SortBy(keys) = mid.as_ref() {
                            let k = effective_topk(Some(*n));
                            return run_sort_by(doc, upstream, keys, k, input, sink);
                        }
                    }
                    return run_limit(doc, l, *n, input, sink);
                }
                Ast::Distinct => return run_distinct(doc, l, input, sink),
                Ast::AggregateBlock { group, reductions, outputs } => {
                    return run_aggregate_block(
                        doc,
                        l,
                        group.as_ref(),
                        reductions,
                        outputs,
                        input,
                        sink,
                    );
                }
                _ => {}
            }
            walk(doc, l, input, &mut |mid| walk(doc, r, mid, sink))
        }

        Ast::Round { value, precision } => {
            // Both args collapse to their first emission, matching how
            // `Binary` and `Neg` treat operands. Non-numeric inputs
            // produce null; null precision defaults to 0 (integer
            // rounding). Negative precision rounds to tens / hundreds.
            let v = to_number(doc, first_emission(doc, value, input.clone()));
            let p_raw = match precision {
                Some(p) => to_number(doc, first_emission(doc, p, input)),
                None => Some(0.0),
            };
            let result: Option<f64> = match (v, p_raw) {
                (Some(x), Some(p)) if p.is_finite() => {
                    // Clamp precision to a sane range so 10^p stays
                    // representable. f64 mantissa has ~15-16 digits;
                    // beyond that the multiply/divide round-trip
                    // loses what little precision the user asked for.
                    let p_int = p.round() as i32;
                    let clamped = p_int.clamp(-308, 308);
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
                Some(n) if n.is_finite() => sink(Value::Number(n)),
                _ => sink(Value::Null),
            }
        }

        Ast::TypeTest { value, kind, negated } => {
            // For each emission of `value`, emit one `Bool` whose
            // truthiness reflects whether the emission's JSON type
            // matches `kind` (or doesn't, when `negated`). Empty
            // emissions emit no bools — surrounding `Select` treats
            // that as "no truthy emission", which mirrors `==`.
            walk(doc, value, input, &mut |v| {
                let matched = value_kind_matches(doc, &v, *kind);
                let result = if *negated { !matched } else { matched };
                sink(Value::Bool(result))
            })
        }

        Ast::Compare(l, op, r) => {
            let mut left_vals: Vec<Value> = Vec::new();
            walk(doc, l, input.clone(), &mut |v| {
                left_vals.push(v);
                true
            });
            let mut right_vals: Vec<Value> = Vec::new();
            walk(doc, r, input, &mut |v| {
                right_vals.push(v);
                true
            });
            for lv in &left_vals {
                for rv in &right_vals {
                    if !sink(Value::Bool(compare_values(doc, lv, *op, rv))) {
                        return false;
                    }
                }
            }
            true
        }

        // Short-circuit `and`. Stops walking each side at the first
        // truthy emission, and emits exactly one `Bool` per And —
        // sufficient because the only consumer (`Select`) cares whether
        // *any* emission was truthy, not the cardinality of truthy
        // emissions.
        Ast::And(l, r) => {
            if !any_truthy(doc, l, input.clone()) {
                return sink(Value::Bool(false));
            }
            sink(Value::Bool(any_truthy(doc, r, input)))
        }
        Ast::Or(l, r) => {
            if any_truthy(doc, l, input.clone()) {
                return sink(Value::Bool(true));
            }
            sink(Value::Bool(any_truthy(doc, r, input)))
        }

        Ast::Not => sink(Value::Bool(!input.is_truthy(doc))),

        Ast::Select(cond) => {
            if any_truthy(doc, cond, input.clone()) { sink(input) } else { true }
        }

        // Outside a pipe-RHS position, reducers / `by` / sort / limit
        // / distinct / aggregate-block all act as identity. The
        // stateful logic lives in the Pipe handler so they can hold
        // state across the upstream's outputs.
        Ast::Sum
        | Ast::Min
        | Ast::Max
        | Ast::Avg
        | Ast::Count
        | Ast::By(..)
        | Ast::SortBy(..)
        | Ast::Limit(..)
        | Ast::Distinct
        | Ast::AggregateBlock { .. } => sink(input),

        // Foreign-reference resolver. Treats the input as a key value
        // and emits each node bucketed under it in the (source, key)
        // foreign-key index. A missing index records an
        // `EvalError::MissingIndex` and aborts the query so the caller
        // can offer to build it.
        Ast::Lookup { source_canon, key_canon, .. } => {
            run_lookup(doc, source_canon, key_canon, input, sink)
        }

        // Postfix `EXPR exists` — true iff EXPR emits at least one
        // value. Stops the inner walk as soon as it sees one emission,
        // so the cost is proportional to "find first hit" rather than
        // a full enumeration. Distinct from `!= null` since an emitted
        // null still counts as exists.
        Ast::Exists(inner) => {
            let mut found = false;
            walk(doc, inner, input, &mut |_| {
                found = true;
                false
            });
            sink(Value::Bool(found))
        }

        // Source-emission counter. Forwards each value unchanged but
        // bumps a thread-local for stats reporting. Accumulates into a
        // local count first so we touch the thread-local once at the
        // end rather than per emission — matters when a source emits
        // millions of rows.
        Ast::Tap(inner) => {
            // Source-emission counter. Forwards each value unchanged
            // but bumps row/byte counters. Bytes only accumulate for
            // `Value::Node` emissions — synthetic values weren't read
            // from the source mmap, so they wouldn't be honest input
            // for a "what did we read from disk?" metric.
            let mut local_rows: u64 = 0;
            let mut local_bytes: u64 = 0;
            let r = walk(doc, inner, input, &mut |v| {
                local_rows += 1;
                if let Value::Node(id) = v {
                    if let Some(rec) = doc.record(id) {
                        local_bytes = local_bytes.saturating_add(rec.length as u64);
                    }
                }
                sink(v)
            });
            if local_rows > 0 {
                bump_scanned_rows(local_rows);
            }
            if local_bytes > 0 {
                bump_scanned_bytes(local_bytes);
            }
            r
        }

        // Per-row binding. Evaluate `value` against the current input
        // (taking the first emission, defaulting to Null on miss),
        // store under `name`, then forward `input` unchanged. The
        // binding outlives the call: it stays in `BINDINGS` until the
        // next `Let` for the same name overwrites it, or the next
        // `evaluator::run` clears the map. That's what makes a `with`
        // binding visible to downstream reducers' per-row callbacks
        // even though `Let` itself has long since returned.
        Ast::Let { name, value } => {
            let bound = first_emission(doc, value, input.clone()).unwrap_or(Value::Null);
            binding_set(name, bound);
            sink(input)
        }

        // Reads the current binding by name. An unbound `Var` emits
        // nothing — happens only if the surface lowers a name that
        // wasn't declared via `with`, which the parser rejects, so
        // this branch is mostly defensive.
        Ast::Var(name) => match binding_get(name) {
            Some(v) => sink(v),
            None => true,
        },

        // Fused field-set equality. One pass over the object's child
        // chain — short-circuits on first mismatch and on first
        // "all-fields-found" signal. Skips the
        // `decode_json_string`/`Scalar::Str` allocation per compare
        // when the target is a literal-shaped string.
        Ast::FieldSetEquals { base, fields, target } => {
            let Some(target_val) = first_emission(doc, target, input.clone()) else {
                return sink(Value::Bool(false));
            };
            let obj_id = first_node_of_kind(doc, base, input.clone(), NodeKind::Object);
            let Some(id) = obj_id else {
                return sink(Value::Bool(false));
            };
            let ok = scan_field_set_equals(doc, id, fields, &target_val, |a, b| {
                fast_equal(doc, a, b)
            });
            sink(Value::Bool(ok))
        }

        // Object projection. For each named field, evaluate the AST
        // against the current input and capture the *first* emission;
        // a missing field becomes `Value::Null`. Emits one synthetic
        // `Value::Object` per input value. Stream-valued field
        // expressions collapse to their first emission — the surface
        // doesn't yet expose array-collection on a projection field.
        Ast::Project(fields) => {
            let mut out: Vec<(String, Value)> = Vec::with_capacity(fields.len());
            for (name, expr) in fields {
                let v = first_emission(doc, expr, input.clone()).unwrap_or(Value::Null);
                out.push((name.clone(), v));
            }
            sink(Value::Object(out))
        }

        // Composite group key. Each component is evaluated against the
        // current input; the *first* emitted value of each (matching
        // the single-key `by` convention) is rendered as a group-key
        // string, and the parts are joined with U+001F. The result is
        // a single synthetic `Value::Str` — surrounding `By` /
        // `AggregateBlock` arms pick it up via `ScalarKey::from_value`
        // unchanged.
        Ast::Binary { op, lhs, rhs } => {
            // Both sides emit at most one value (first emission wins).
            // Non-numeric operands and divide-by-zero produce null.
            let l = first_emission(doc, lhs, input.clone());
            let r = first_emission(doc, rhs, input);
            sink(eval_arith(doc, *op, l, r))
        }
        Ast::Neg(inner) => {
            let v = first_emission(doc, inner, input);
            sink(match to_number(doc, v) {
                Some(n) => Value::Number(-n),
                None => Value::Null,
            })
        }
        Ast::ReducerSlot(i) => {
            // Reads the finalised value for slot `i` of the currently-
            // emitting aggregate bucket. The aggregate evaluator pushes
            // a slot frame before walking each bucket's output
            // expressions. Outside that scope (or with an out-of-range
            // index) the slot is treated as null, which propagates
            // through arithmetic to a null result.
            match reducer_slot_value(*i) {
                Some(n) => sink(Value::Number(n)),
                None => sink(Value::Null),
            }
        }

        // Object literal — same per-field evaluation as `Project`, but
        // surfacing the value directly rather than as a row in a
        // projection step. Each field's expression collapses to its
        // first emission; missing emissions render as `null`.
        Ast::Object(fields) => {
            let mut out: Vec<(String, Value)> = Vec::with_capacity(fields.len());
            for (name, expr) in fields {
                let v = first_emission(doc, expr, input.clone()).unwrap_or(Value::Null);
                out.push((name.clone(), v));
            }
            sink(Value::Object(out))
        }

        Ast::KeyTuple(parts) => {
            let mut joined = String::new();
            for (i, p) in parts.iter().enumerate() {
                if i > 0 {
                    joined.push('\u{1F}');
                }
                // Missing component renders as empty — keeps the
                // tuple's column count stable across rows.
                if let Some(v) = first_emission(doc, p, input.clone()) {
                    joined.push_str(&value_to_group_key(doc, &v));
                }
            }
            sink(Value::Str(joined))
        }
    }
}

// ----- small per-arm helpers -----

/// Coerce a Value to a finite f64, returning None for null / non-numeric
/// / non-finite. Used by `Ast::Binary` and `Ast::Neg` to enforce the
/// "numeric or null" contract across arithmetic.
pub(super) fn to_number(doc: &Document, v: Option<Value>) -> Option<f64> {
    let v = v?;
    match v {
        Value::Number(n) if n.is_finite() => Some(n),
        Value::Group { n: Some(n), .. } if n.is_finite() => Some(n),
        Value::Node(id) => {
            if matches!(doc.node_kind(id), crate::document::NodeKind::Number) {
                let bytes = doc.value_bytes(id)?;
                let s = std::str::from_utf8(bytes).ok()?;
                let n: f64 = s.parse().ok()?;
                if n.is_finite() { Some(n) } else { None }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Evaluate a binary arithmetic op on two coerced operands. Any non-
/// numeric input or div-by-zero collapses to `Value::Null`.
pub(super) fn eval_arith(doc: &Document, op: BinaryOp, l: Option<Value>, r: Option<Value>) -> Value {
    let (Some(a), Some(b)) = (to_number(doc, l), to_number(doc, r)) else {
        return Value::Null;
    };
    let result = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => {
            if b == 0.0 {
                return Value::Null;
            }
            a / b
        }
    };
    if result.is_finite() {
        Value::Number(result)
    } else {
        Value::Null
    }
}

/// Walks `expr` and returns its first emission, if any.
pub(super) fn first_emission(doc: &Document, expr: &Ast, input: Value) -> Option<Value> {
    let mut found: Option<Value> = None;
    walk(doc, expr, input, &mut |v| {
        if found.is_none() {
            found = Some(v);
        }
        false
    });
    found
}

/// Does the value match a given JSON type? Resolves document nodes
/// through `doc.node_kind`; for synthetic values, dispatches on the
/// `Value` variant. `Group` aggregates count as numbers (or null when
/// empty); `GroupList` and `BucketRow` count as arrays/objects to
/// match how the renderer treats them.
fn value_kind_matches(
    doc: &Document,
    v: &Value,
    kind: super::super::ast::JsonTypeKind,
) -> bool {
    use super::super::ast::JsonTypeKind as K;
    let actual: K = match v {
        Value::Null => K::Null,
        Value::Bool(_) => K::Bool,
        Value::Number(_) => K::Number,
        Value::Str(_) => K::String,
        Value::Group { n: Some(_), .. } => K::Number,
        Value::Group { n: None, .. } => K::Null,
        Value::GroupList { .. } => K::Array,
        Value::Object(_) => K::Object,
        Value::BucketRow(_) => K::Object,
        Value::NamedValue { value, .. } => return value_kind_matches(doc, value, kind),
        Value::Node(id) => {
            use crate::document::NodeKind;
            match doc.node_kind(*id) {
                NodeKind::Null   => K::Null,
                NodeKind::Bool   => K::Bool,
                NodeKind::Number => K::Number,
                NodeKind::String => K::String,
                NodeKind::Array  => K::Array,
                NodeKind::Object => K::Object,
            }
        }
    };
    actual == kind
}

/// True iff `expr` emits at least one truthy value when run against `input`.
fn any_truthy(doc: &Document, expr: &Ast, input: Value) -> bool {
    let mut yes = false;
    walk(doc, expr, input, &mut |v| {
        if v.is_truthy(doc) {
            yes = true;
            false
        } else {
            true
        }
    });
    yes
}

/// Returns the first `Value::Node` whose record has `expected_kind`.
fn first_node_of_kind(
    doc: &Document,
    expr: &Ast,
    input: Value,
    expected_kind: NodeKind,
) -> Option<u32> {
    let mut found: Option<u32> = None;
    walk(doc, expr, input, &mut |v| {
        if found.is_none() {
            if let Value::Node(id) = v {
                if doc.node_kind(id) == expected_kind {
                    found = Some(id);
                }
            }
        }
        false
    });
    found
}

/// Effective top-K for a sort_by stage. Combines an optional explicit
/// `limit n` (when the surface produced `... order by ... limit n`)
/// with the outer query result cap set by `evaluator::run`. Either may
/// be absent: a missing explicit limit means "take K = outer cap"; a
/// missing outer cap (only happens for `walk_eval` invoked outside
/// `run`) means "K = explicit limit, or unbounded if neither is set".
fn effective_topk(explicit: Option<u64>) -> usize {
    let outer = outer_limit();
    let outer_cap = if outer == 0 { usize::MAX } else { outer };
    match explicit {
        None => outer_cap,
        Some(n) => {
            let n = if n > usize::MAX as u64 { usize::MAX } else { n as usize };
            n.min(outer_cap)
        }
    }
}

fn run_limit(
    doc: &Document,
    l: &Ast,
    cap: u64,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let mut count: u64 = 0;
    walk(doc, l, input, &mut |v| {
        if count >= cap {
            return false;
        }
        count += 1;
        sink(v)
    })
}

fn run_distinct(
    doc: &Document,
    l: &Ast,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    // `seen` is local to this pipe so two `distinct` stages in one
    // query don't share state. Hot-loop contract for high-cardinality
    // streams:
    //   * `scratch` is reused across rows so the fingerprint buffer
    //     amortises to a handful of `Vec` reallocations rather than
    //     one per row.
    //   * `seen.contains(&[u8])` borrows the scratch buffer
    //     (`Vec<u8>: Borrow<[u8]>`), so a hit allocates nothing; only
    //     first-sight rows pay the `clone` to insert.
    //   * `FxBuildHasher` — fast, non-cryptographic; safe here because
    //     the hash table is local.
    let mut seen: std::collections::HashSet<Vec<u8>, rustc_hash::FxBuildHasher> =
        std::collections::HashSet::default();
    let mut scratch: Vec<u8> = Vec::with_capacity(32);
    walk(doc, l, input, &mut |v| {
        scratch.clear();
        write_fingerprint(doc, &v, &mut scratch);
        if seen.contains(scratch.as_slice()) {
            true
        } else {
            seen.insert(scratch.clone());
            sink(v)
        }
    })
}

fn run_lookup(
    doc: &Document,
    source_canon: &str,
    key_canon: &str,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    // Fast path: `evaluator::run` pre-resolves every reachable Lookup
    // against the registry while holding the index mutex. The hot
    // loop pays no per-row mutex acquire and no per-row HashMap fetch.
    let cache_key = source_canon.as_ptr() as usize;
    match lookup_resolved(cache_key) {
        ResolvedIndex::Missing => {
            set_eval_error(EvalError::MissingIndex {
                source: source_canon.to_string(),
                key: key_canon.to_string(),
            });
            return false;
        }
        ResolvedIndex::Hit(idx) => {
            bump_lookup_calls();
            let target = match ScalarKey::from_value(doc, &input) {
                Some(k) => k,
                None => return true,
            };
            if let Some(ids) = idx.get(&target) {
                for &id in ids {
                    if !sink(Value::Node(id)) {
                        return false;
                    }
                }
            }
            return true;
        }
        ResolvedIndex::Unresolved => {
            // `walk_eval` invoked outside `run` (index builder).
            // Fall through to the locking path.
        }
    }
    let registry = match doc.indexes.lock() {
        Ok(g) => g,
        Err(_) => {
            set_eval_error(EvalError::MissingIndex {
                source: source_canon.to_string(),
                key: key_canon.to_string(),
            });
            return false;
        }
    };
    let Some(idx) = registry.get(source_canon, key_canon) else {
        set_eval_error(EvalError::MissingIndex {
            source: source_canon.to_string(),
            key: key_canon.to_string(),
        });
        return false;
    };
    bump_lookup_calls();
    let target = match ScalarKey::from_value(doc, &input) {
        Some(k) => k,
        None => return true,
    };
    if let Some(ids) = idx.get(&target) {
        for &id in ids {
            if !sink(Value::Node(id)) {
                return false;
            }
        }
    }
    true
}
