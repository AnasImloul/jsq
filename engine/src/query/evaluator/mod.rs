//! Engine-aware query evaluator. Walks the index directly via the
//! pre-order `subtree_size` invariant — no JSONNode wrappers in the
//! loop. Sibling pointers are derived (`id + subtree_size`); subtrees
//! are contiguous slices of the records array.
//!
//! Evaluation strategy: push-based. `walk` invokes a sink for each value
//! it produces. The sink can return false to stop iteration (limit hit).
//! Sub-evaluations are wired together by passing the parent's sink down
//! into a closure for the inner pass.
//!
//! Module layout:
//! - [`walk`]: the AST dispatch — also home to the reducer / aggregate /
//!   sort / fingerprint helpers, since they're tightly coupled to the
//!   per-row callback shape.
//! - [`scan`]: one-level source-byte scanners that surface primitive
//!   children (which don't have records under the hybrid emit-gate)
//!   and the descent helpers.
//! - [`render`]: result-row formatting (the path/preview/full_text
//!   triple seen by Swift) and number/string escapers.
//!
//! This file owns only the public surface (`run`, `collect_kinds`,
//! `collect_keys`, `text_search`), the per-run thread-local state, and
//! the `EvalError` / `QueryResult` / `EvalOutput` types crossed back
//! into the FFI layer.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::document::{Document, NodeKind, FLAG_OBJECT_MEMBER};

use super::index::ForeignKeyIndex;
use super::value::Value;

mod aggregate;
mod compare;
mod fingerprint;
mod functions;
mod reducers;
pub mod render;
mod scan;
mod sort;
mod walk;

pub use walk::walk_eval;

/// Errors raised mid-evaluation. Currently only `MissingIndex` — a
/// `lookup(SOURCE; KEY)` was hit at runtime with no matching entry in
/// the document's index registry. The query is aborted (no partial
/// results emitted) and the error is propagated to the caller so the
/// UI can offer to create the index.
#[derive(Clone, Debug)]
pub enum EvalError {
    MissingIndex { source: String, key: String },
}

thread_local! {
    static EVAL_ERROR: RefCell<Option<EvalError>> = const { RefCell::new(None) };
    /// Bumped by `Ast::Tap` for each row that flows out of the source
    /// path. Reset at the start of every `evaluator::run`; sampled at
    /// the end and surfaced via `EvalOutput.scanned_rows`. TLS rather
    /// than a process-wide atomic so concurrent `evaluator::run`
    /// calls (e.g. test parallelism) don't clobber each other.
    static SCANNED_ROWS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The outer row cap passed to `evaluator::run`. Read by the sort
    /// arm to size its top-K heap when the query has no explicit
    /// trailing `limit`. Zero ⇒ unset (e.g. for `walk_eval` invoked
    /// outside `run`); the sort falls back to an unbounded buffer in
    /// that case so existing callers see no behaviour change.
    static OUTER_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Bumped on every `Ast::Lookup` invocation that finds its index.
    static LOOKUP_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Accumulates source byte spans of nodes that flowed through
    /// `Ast::Tap` (the source-emission counter). A rough proxy for
    /// the working-set size the query touched — useful for spotting
    /// memory-bandwidth-bound queries. Reset / sampled in lockstep
    /// with `SCANNED_ROWS`.
    static SCANNED_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Per-row bindings written by `Ast::Let` and read by `Ast::Var`.
    /// Cleared at the start of every `evaluator::run`; otherwise
    /// `Let` invocations *overwrite* on each pass — no scope-stack,
    /// no pop. That's what makes downstream reducers (which run their
    /// own per-row callbacks after `Let` has returned) able to see
    /// the current row's binding.
    static BINDINGS: RefCell<HashMap<String, Value>> =
        RefCell::new(HashMap::new());
    /// Finalised reducer slot values for the aggregate-block bucket
    /// currently emitting. `Ast::ReducerSlot(i)` reads slot `i` from
    /// this stack. The aggregate evaluator pushes a bucket's slot
    /// vector before walking the bucket's output expressions and pops
    /// it afterwards, so nested aggregates (when they arrive) keep
    /// separate slot frames without cross-talk.
    static REDUCER_SLOTS: RefCell<Vec<Vec<Option<f64>>>> = const { RefCell::new(Vec::new()) };

    /// Document root node id for the current `run`. Read by the
    /// `Ast::Subquery` arm: a correlated subquery evaluates against the
    /// document root (with the outer row's aliases still bound) rather
    /// than against the current pipeline input. Set at the top of `run`
    /// and left at 0 otherwise — 0 is the conventional root, which is
    /// also what `collect_kinds` / `collect_keys` seed, so those paths
    /// see a consistent root.
    static DOC_ROOT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };

    /// Resolved foreign-key indexes for every `Ast::Lookup` reachable
    /// from the query root. Populated once at the top of `run` while
    /// the document's index mutex is held; consulted by the `Lookup`
    /// arm with no additional locking. The map key is the address of
    /// the lookup's `source_canon` string — stable for the lifetime of
    /// the AST and unique per Lookup node.
    ///
    /// Stored values are pointer-as-usize. A non-zero value is a valid
    /// `*const ForeignKeyIndex` whose pointee outlives this run (the
    /// guard is held until the map is cleared). A zero value is a
    /// negative-cache entry — "we checked at run-start and the index
    /// wasn't registered" — so the `Lookup` arm can raise
    /// `MissingIndex` without re-locking.
    static LOOKUP_RESOLVED: RefCell<HashMap<usize, usize>> =
        RefCell::new(HashMap::new());
}

pub(super) fn set_eval_error(err: EvalError) {
    EVAL_ERROR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    });
}

fn take_eval_error() -> Option<EvalError> {
    EVAL_ERROR.with(|cell| cell.borrow_mut().take())
}

fn reset_scanned_rows() {
    SCANNED_ROWS.with(|c| c.set(0));
}

fn take_scanned_rows() -> u64 {
    SCANNED_ROWS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

pub(super) fn bump_scanned_rows(by: u64) {
    SCANNED_ROWS.with(|c| c.set(c.get().saturating_add(by)));
}

pub(super) fn outer_limit() -> usize {
    OUTER_LIMIT.with(|c| c.get())
}

fn reset_lookup_calls() {
    LOOKUP_CALLS.with(|c| c.set(0));
}

fn take_lookup_calls() -> u64 {
    LOOKUP_CALLS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

pub(super) fn bump_lookup_calls() {
    LOOKUP_CALLS.with(|c| c.set(c.get().saturating_add(1)));
}

fn reset_scanned_bytes() {
    SCANNED_BYTES.with(|c| c.set(0));
}

fn take_scanned_bytes() -> u64 {
    SCANNED_BYTES.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

pub(super) fn bump_scanned_bytes(by: u64) {
    SCANNED_BYTES.with(|c| c.set(c.get().saturating_add(by)));
}

pub(super) fn push_reducer_slots(slots: Vec<Option<f64>>) {
    REDUCER_SLOTS.with(|s| s.borrow_mut().push(slots));
}

pub(super) fn pop_reducer_slots() {
    REDUCER_SLOTS.with(|s| {
        s.borrow_mut().pop();
    });
}

pub(super) fn doc_root() -> u32 {
    DOC_ROOT.with(|c| c.get())
}

pub(super) fn reducer_slot_value(i: usize) -> Option<f64> {
    REDUCER_SLOTS.with(|s| {
        let stack = s.borrow();
        stack.last().and_then(|top| top.get(i).copied().flatten())
    })
}

fn reset_reducer_slots() {
    REDUCER_SLOTS.with(|s| s.borrow_mut().clear());
}

fn reset_bindings() {
    BINDINGS.with(|b| b.borrow_mut().clear());
}

pub(super) fn binding_set(name: &str, value: Value) {
    BINDINGS.with(|b| {
        b.borrow_mut().insert(name.to_string(), value);
    });
}

pub(super) fn binding_get(name: &str) -> Option<Value> {
    BINDINGS.with(|b| b.borrow().get(name).cloned())
}

/// Outcome of `LOOKUP_RESOLVED` lookup for a `Lookup` AST node.
pub(super) enum ResolvedIndex<'a> {
    /// Pre-resolution found the index in the registry. Borrowed for
    /// the lifetime of `evaluator::run`'s registry guard.
    Hit(&'a ForeignKeyIndex),
    /// Pre-resolution checked the registry at run-start and the index
    /// wasn't there; the caller should raise `MissingIndex`.
    Missing,
    /// `walk_eval` was called outside `run` and pre-resolution didn't
    /// reach this node — the caller falls back to a locking lookup.
    Unresolved,
}

/// Reads the cached resolution for a `Lookup` node.
///
/// SAFETY contract for [`ResolvedIndex::Hit`]: `evaluator::run` writes
/// the pointer while holding the document's index mutex guard and
/// clears the map *before* dropping the guard, so any non-zero entry
/// in the map points at a `ForeignKeyIndex` still owned by the
/// registry. Callers reach the `Hit` arm only by going through this
/// function, which keeps the cast-and-deref fully encapsulated.
pub(super) fn lookup_resolved<'a>(canon_key: usize) -> ResolvedIndex<'a> {
    let raw = LOOKUP_RESOLVED.with(|r| r.borrow().get(&canon_key).copied());
    match raw {
        Some(0) => ResolvedIndex::Missing,
        Some(p) => {
            let ptr = p as *const ForeignKeyIndex;
            // SAFETY: see the function-level contract above.
            ResolvedIndex::Hit(unsafe { &*ptr })
        }
        None => ResolvedIndex::Unresolved,
    }
}

/// Builds and registers a foreign-key index for every `Ast::Lookup`
/// reachable from `ast` that isn't already in the registry.
///
/// The Swift app builds indexes explicitly over FFI and reuses them
/// across many queries against the same document; the `jsq` CLI runs a
/// single query and exits, so it calls this once to make joins work
/// without a separate build step. Deduplicates by canonical
/// `(source, key)` so repeated joins on the same key build once, and
/// skips any pair already present so a caller that pre-built some
/// indexes keeps them.
pub fn build_indexes(doc: &Document, ast: &super::ast::Ast) {
    let mut nodes: Vec<&super::ast::Ast> = Vec::new();
    collect_lookups(ast, &mut nodes);
    let mut built: HashSet<(String, String)> = HashSet::new();
    for node in nodes {
        let super::ast::Ast::Lookup { source, key, source_canon, key_canon } = node else {
            continue;
        };
        if !built.insert((source_canon.clone(), key_canon.clone())) {
            continue;
        }
        if let Ok(reg) = doc.indexes.lock() {
            if reg.get(source_canon, key_canon).is_some() {
                continue;
            }
        }
        let index = ForeignKeyIndex::build(doc, source, key);
        if let Ok(mut reg) = doc.indexes.lock() {
            reg.insert(source_canon.clone(), key_canon.clone(), index);
        }
    }
}

/// Walks the AST and collects every `Ast::Lookup` node it finds. Both
/// the per-run index pre-resolution (which reads each node's
/// `source_canon`/`key_canon`) and the CLI's index auto-build (which
/// reads each node's `source`/`key` sub-ASTs) share this single
/// traversal.
pub(crate) fn collect_lookups<'a>(
    ast: &'a super::ast::Ast,
    out: &mut Vec<&'a super::ast::Ast>,
) {
    use super::ast::Ast;
    match ast {
        Ast::Lookup { source, key, .. } => {
            out.push(ast);
            // Recurse into source/key — the surface only emits flat
            // Lookups today, but a nested one in either arm would
            // otherwise silently bypass pre-resolution and lock the
            // index registry per row.
            collect_lookups(source, out);
            collect_lookups(key, out);
        }
        Ast::JoinEach { outer_key, lookup, .. } => {
            collect_lookups(outer_key, out);
            collect_lookups(lookup, out);
        }
        Ast::UnnestEach { source, .. } => collect_lookups(source, out),
        // Recurse into the subquery's pipeline so any join/lookup inside
        // a correlated subquery is pre-resolved (and CLI auto-built) just
        // like one in the outer query — otherwise it would lock the index
        // registry per outer row, or fail to build at all under `jsq`.
        Ast::Subquery { pipeline } => collect_lookups(pipeline, out),
        Ast::Pipe(l, r)
        | Ast::And(l, r)
        | Ast::Or(l, r)
        | Ast::Compare(l, _, r)
        | Ast::By(l, r) => {
            collect_lookups(l, out);
            collect_lookups(r, out);
        }
        Ast::Select(inner) | Ast::Exists(inner) | Ast::Tap(inner) => {
            collect_lookups(inner, out);
        }
        Ast::Let { value, .. } => collect_lookups(value, out),
        Ast::FieldSetEquals { base, target, .. } => {
            collect_lookups(base, out);
            collect_lookups(target, out);
        }
        Ast::Project(fields) => {
            for (_, e) in fields {
                collect_lookups(e, out);
            }
        }
        Ast::SortBy(keys) => {
            for (k, _) in keys {
                collect_lookups(k, out);
            }
        }
        Ast::KeyTuple(parts) | Ast::ArrayLit(parts) => {
            for p in parts {
                collect_lookups(p, out);
            }
        }
        Ast::AggregateBlock { group, reductions, outputs } => {
            match group {
                Some(super::ast::AggGroup::Single { key, .. }) => collect_lookups(key, out),
                Some(super::ast::AggGroup::Rollup(keys)) => {
                    for k in keys {
                        collect_lookups(&k.key, out);
                    }
                }
                None => {}
            }
            for r in reductions {
                if let Some(v) = &r.value {
                    collect_lookups(v, out);
                }
                if let Some(p) = &r.where_pred {
                    collect_lookups(p, out);
                }
            }
            for (_, node) in outputs {
                collect_lookups_in_agg_output(node, out);
            }
        }
        Ast::Binary { lhs, rhs, .. } => {
            collect_lookups(lhs, out);
            collect_lookups(rhs, out);
        }
        Ast::Neg(inner) => collect_lookups(inner, out),
        Ast::Object(fields) => {
            for (_, v) in fields {
                collect_lookups(v, out);
            }
        }
        Ast::TypeTest { value, .. } => collect_lookups(value, out),
        Ast::Call { args, .. } => {
            for a in args {
                collect_lookups(a, out);
            }
        }
        Ast::If { cond, then_branch, else_branch } => {
            collect_lookups(cond, out);
            collect_lookups(then_branch, out);
            collect_lookups(else_branch, out);
        }
        // Leaves with no nested expressions.
        Ast::Identity
        | Ast::Field(_)
        | Ast::Index(_)
        | Ast::Iterate
        | Ast::Descend
        | Ast::DescendField(_)
        | Ast::IterateField(_)
        | Ast::LitNumber(_)
        | Ast::LitString(_)
        | Ast::LitBool(_)
        | Ast::LitNull
        | Ast::Not
        | Ast::Sum
        | Ast::Min
        | Ast::Max
        | Ast::Avg
        | Ast::Count
        | Ast::Limit(_)
        | Ast::Distinct
        | Ast::Var(_)
        | Ast::ReducerSlot(_) => {}
    }
}

fn collect_lookups_in_agg_output<'a>(
    node: &'a super::ast::AggOutputNode,
    out: &mut Vec<&'a super::ast::Ast>,
) {
    match node {
        super::ast::AggOutputNode::Leaf { expr, default } => {
            collect_lookups(expr, out);
            if let Some(d) = default {
                collect_lookups(d, out);
            }
        }
        super::ast::AggOutputNode::Object(fields) => {
            for (_, child) in fields {
                collect_lookups_in_agg_output(child, out);
            }
        }
    }
}

/// Result row crossed back to FFI / Swift. Carries the row's value
/// directly; presentation strings (preview, full JSON) are derived on
/// demand by the renderer or the FFI marshalling layer. `kind` and
/// `path` are cached because both consumers (CSV type column, Swift
/// table view) read them per-row and recomputing the path means
/// re-walking the ancestor chain.
#[derive(Clone, Debug)]
pub struct QueryResult {
    pub kind: u8, // ENGINE_KIND_*
    pub path: String,
    pub value: super::value::Value,
}

#[derive(Debug)]
pub struct EvalOutput {
    pub results: Vec<QueryResult>,
    pub hit_limit: bool,
    pub error: Option<EvalError>,
    /// How many rows the source path produced before the rest of the
    /// pipeline (filter / aggregate / sort / limit) ran. The number
    /// the user wants when asking "how much data did this query
    /// touch?". Zero for queries whose source emits nothing or that
    /// hit a parse error before evaluation started.
    pub scanned_rows: u64,
    /// How many `lookup(...)` invocations resolved against the index
    /// registry during this run. A field-set like `dim.{a, b, c} == V`
    /// fans out to one lookup per field per row; this counter shows
    /// the actual workload that produced.
    pub lookup_calls: u64,
    /// Source byte span of every node the source path emitted —
    /// summed across all rows that flowed through `Ast::Tap`. Compare
    /// against the file size to spot memory-bandwidth-bound queries:
    /// when this approaches the document size the engine touched
    /// most of the file regardless of how few rows survived later
    /// filtering.
    pub scanned_bytes: u64,
}

pub fn run(doc: &Document, ast: &super::ast::Ast, root: u32, limit: usize) -> EvalOutput {
    let _ = take_eval_error(); // clear stale state from previous runs
    reset_scanned_rows();
    reset_scanned_bytes();
    reset_lookup_calls();
    reset_bindings();
    reset_reducer_slots();
    OUTER_LIMIT.with(|c| c.set(limit));
    DOC_ROOT.with(|c| c.set(root));

    // Pre-resolve every Ast::Lookup against the registry once, while
    // we hold the document's index mutex. The Lookup arm then reads a
    // raw pointer out of `LOOKUP_RESOLVED` per row — no per-call lock,
    // no per-call canonical-string HashMap fetch. The mutex guard is
    // held until we clear `LOOKUP_RESOLVED` below, so all stored
    // pointers remain valid for the duration of the walk.
    let mut lookup_refs: Vec<&super::ast::Ast> = Vec::new();
    collect_lookups(ast, &mut lookup_refs);
    let _registry_guard = if lookup_refs.is_empty() {
        None
    } else {
        match doc.indexes.lock() {
            Ok(g) => {
                let mut resolved: HashMap<usize, usize> =
                    HashMap::with_capacity(lookup_refs.len());
                for node in &lookup_refs {
                    let super::ast::Ast::Lookup { source_canon, key_canon, .. } = node else {
                        continue;
                    };
                    let p = match g.get(source_canon, key_canon) {
                        Some(idx) => idx as *const ForeignKeyIndex as usize,
                        None => 0, // negative cache — Lookup arm raises MissingIndex
                    };
                    resolved.insert(source_canon.as_ptr() as usize, p);
                }
                LOOKUP_RESOLVED.with(|r| *r.borrow_mut() = resolved);
                Some(g)
            }
            Err(_) => None,
        }
    };

    let mut results: Vec<QueryResult> = Vec::new();
    let mut counter: usize = 0;
    let mut sink = |v: Value| -> bool {
        if counter >= limit {
            counter += 1;
            return false;
        }
        results.push(render::make_result(doc, &v));
        counter += 1;
        true
    };
    let _ = walk::walk(doc, ast, Value::Node(root), &mut sink);

    // Drop cached pointers *before* releasing the guard so we can never
    // observe stale pointers from a future run.
    LOOKUP_RESOLVED.with(|r| r.borrow_mut().clear());
    drop(_registry_guard);

    OUTER_LIMIT.with(|c| c.set(0));
    DOC_ROOT.with(|c| c.set(0));
    let error = take_eval_error();
    let scanned_rows = take_scanned_rows();
    let scanned_bytes = take_scanned_bytes();
    let lookup_calls = take_lookup_calls();
    // On error, drop partial results — the query as a whole is invalid.
    let (results, hit_limit) = if error.is_some() {
        (Vec::new(), false)
    } else {
        (results, counter > limit)
    };
    EvalOutput {
        results,
        hit_limit,
        error,
        scanned_rows,
        lookup_calls,
        scanned_bytes,
    }
}

/// Returns a bitmask of the NodeKind values seen across the first
/// `limit` outputs of `ast`. Bit positions match `NodeKind as u8`:
///   bit 0 = null, 1 = bool, 2 = number, 3 = string, 4 = array,
///   5 = object. Used by autocomplete to switch between object-key
/// suggestions and array-accessor suggestions.
pub fn collect_kinds(doc: &Document, ast: &super::ast::Ast, limit: usize) -> u8 {
    let mut bitmask: u8 = 0;
    let mut count = 0usize;
    let _ = walk::walk(doc, ast, Value::Node(0), &mut |v| {
        if count >= limit {
            return false;
        }
        count += 1;
        let kind = match v {
            Value::Node(id) => doc.node_kind(id),
            Value::Null => NodeKind::Null,
            Value::Bool(_) => NodeKind::Bool,
            Value::Number(_) | Value::Group { .. } => NodeKind::Number,
            Value::Str(_) => NodeKind::String,
            Value::GroupList { .. } | Value::Object(_) | Value::BucketRow(_) | Value::Array(_) => NodeKind::Array,
            Value::NamedValue { .. } => NodeKind::Object,
        };
        bitmask |= 1 << (kind as u8);
        true
    });
    bitmask
}

/// Runs `ast` and returns the union of object keys found in its outputs,
/// sampling at most `limit` outputs. Used by schema-aware autocomplete.
///
/// Keys are extracted by scanning the object's source bytes rather than
/// walking the record array, so objects whose properties are all
/// primitives (which under the hybrid emit-gate have no records of
/// their own) still surface their keys.
pub fn collect_keys(doc: &Document, ast: &super::ast::Ast, limit: usize) -> Vec<String> {
    let mut keys: HashSet<String> = HashSet::new();
    let mut count = 0usize;
    let _ = walk::walk(doc, ast, Value::Node(0), &mut |v| {
        if count >= limit {
            return false;
        }
        count += 1;
        if let Value::Node(id) = v {
            if doc.node_kind(id) == NodeKind::Object {
                if let Some(bytes) = doc.value_bytes(id) {
                    extract_object_keys(bytes, &mut keys);
                }
            }
        }
        true
    });
    let mut sorted: Vec<String> = keys.into_iter().collect();
    sorted.sort();
    sorted
}

/// Scans an object's source bytes (`{ ... }`) and inserts every key it
/// finds into `out`. Tolerant of escape sequences (decoded via
/// `parse_string_view`) and of malformed tails (returns early on the
/// first unrecognised byte rather than panicking).
fn extract_object_keys(src: &[u8], out: &mut HashSet<String>) {
    use crate::source_scan::{parse_string_view, skip_inline_value, skip_ws};
    if src.is_empty() || src[0] != b'{' {
        return;
    }
    let mut pos = 1usize;
    loop {
        skip_ws(src, &mut pos);
        if pos >= src.len() || src[pos] == b'}' {
            return;
        }
        let key_bytes = match parse_string_view(src, &mut pos) {
            Some(k) => k,
            None => return,
        };
        if let Ok(s) = std::str::from_utf8(&key_bytes) {
            out.insert(s.to_string());
        }
        skip_ws(src, &mut pos);
        if pos >= src.len() || src[pos] != b':' {
            return;
        }
        pos += 1;
        skip_ws(src, &mut pos);
        skip_inline_value(src, &mut pos);
        skip_ws(src, &mut pos);
        if pos < src.len() && src[pos] == b',' {
            pos += 1;
            continue;
        }
        return;
    }
}

/// Plain-text substring search across the entire document. Walks every
/// node iteratively (no recursion, depth-safe) and emits each one whose
/// key OR primitive value bytes contain `needle`. Case-insensitive ASCII
/// matching — non-ASCII bytes are compared as-is, which is good enough
/// for the "find a label" use case without paying the cost of Unicode
/// case-folding on multi-GB walks.
pub fn text_search(doc: &Document, needle: &[u8], limit: usize) -> EvalOutput {
    let mut results: Vec<QueryResult> = Vec::new();
    if needle.is_empty() {
        return EvalOutput {
            results,
            hit_limit: false,
            error: None,
            scanned_rows: 0,
            lookup_calls: 0,
            scanned_bytes: 0,
        };
    }
    let lower_needle: Vec<u8> = needle.iter().map(|&b| b.to_ascii_lowercase()).collect();
    let records = doc.records();
    let keys = doc.keys();
    let source = &doc.source_mmap[..];

    let mut counter = 0usize;
    // Pre-order layout: iterating the records array is a depth-first
    // walk of the document — no stack needed.
    'scan: for cur in 0..records.len() as u32 {
        let r = &records[cur as usize];

        // Match on object-member key.
        let key_match = if r.flags & FLAG_OBJECT_MEMBER != 0 && r.key_length > 0 {
            let s = r.key_or_index as usize;
            match s.checked_add(r.key_length as usize) {
                Some(e) if e <= keys.len() => byte_contains_ci(&keys[s..e], &lower_needle),
                _ => false,
            }
        } else {
            false
        };

        // Match on primitive value (string / number / bool / null).
        // For strings the value bytes include the surrounding quotes;
        // matching against them is fine for "find substring".
        let kind = NodeKind::from_u8(r.kind);
        let value_match = if !key_match && !matches!(kind, NodeKind::Object | NodeKind::Array) {
            let start = r.offset as usize;
            let end = start + r.length as usize;
            if end <= source.len() {
                byte_contains_ci(&source[start..end], &lower_needle)
            } else {
                false
            }
        } else {
            false
        };

        if key_match || value_match {
            if counter >= limit {
                counter += 1;
                break 'scan;
            }
            results.push(render::make_result(doc, &Value::Node(cur)));
            counter += 1;
        }
    }

    EvalOutput {
        results,
        hit_limit: counter > limit,
        error: None,
        // Text search visits every node in the document tree; that's
        // its scan cost. Mirrors what the structural query path
        // reports for a `**`-style walk.
        scanned_rows: doc.records().len() as u64,
        lookup_calls: 0,
        // Plain-text search reads through the entire source mmap to
        // match against primitive bytes — that's the whole file.
        scanned_bytes: doc.source_mmap.len() as u64,
    }
}

/// ASCII case-insensitive substring search. `needle_lower` is assumed
/// pre-lowercased; we lowercase haystack bytes on the fly during the
/// inner compare. Optimised for short needles (the typical user case).
fn byte_contains_ci(haystack: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() || haystack.len() < needle_lower.len() {
        return false;
    }
    let n = needle_lower.len();
    let last = haystack.len() - n;
    let first = needle_lower[0];
    'outer: for i in 0..=last {
        if haystack[i].to_ascii_lowercase() != first {
            continue;
        }
        for j in 1..n {
            if haystack[i + j].to_ascii_lowercase() != needle_lower[j] {
                continue 'outer;
            }
        }
        return true;
    }
    false
}
