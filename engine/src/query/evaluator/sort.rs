//! Streaming top-K sort — drives the `LHS | sort_by(K1, K2, ...)`
//! postfix. Memory is O(K) over the upstream stream, where K is the
//! effective row cap derived from a downstream `limit n` (when one
//! follows the sort) or the outer query result cap (default 5000).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::document::Document;

use super::super::ast::{Ast, SortDir};
use super::super::value::{Scalar, Value};
use super::walk::walk;

/// Top-K sort. `k` is the maximum number of rows the caller will ever
/// consume; we only retain the K best-by-sort-order rows seen so far,
/// using a max-heap whose root is the *worst* surviving row. Each new
/// row beats the current worst → swap; otherwise we drop it. At end of
/// stream, drain the heap in best-first order and forward downstream.
pub(super) fn run_sort_by(
    doc: &Document,
    upstream: &Ast,
    keys: &[(Box<Ast>, SortDir)],
    k: usize,
    input: Value,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    if k == 0 {
        // Still drive upstream so scanned-row counters reflect what
        // the source actually produced.
        walk(doc, upstream, input, &mut |_| true);
        return true;
    }

    let dirs: Vec<SortDir> = keys.iter().map(|(_, d)| *d).collect();
    let mut heap: BinaryHeap<Entry> = BinaryHeap::with_capacity(k.min(1024));
    let mut seq: u64 = 0;

    walk(doc, upstream, input, &mut |row| {
        let mut key_scalars: Vec<Scalar> = Vec::with_capacity(keys.len());
        for (k_ast, _dir) in keys {
            let mut got: Option<Value> = None;
            walk(doc, k_ast, row.clone(), &mut |v| {
                if got.is_none() {
                    got = Some(v);
                }
                false
            });
            let v = got.unwrap_or(Value::Null);
            key_scalars.push(Scalar::from_value(doc, &v));
        }
        let entry = Entry {
            ord: OrdKey { dirs: dirs.clone(), keys: key_scalars, seq },
            row,
        };
        seq += 1;

        if heap.len() < k {
            heap.push(entry);
        } else if let Some(worst) = heap.peek() {
            // `entry < worst` ⇒ entry should sort *earlier* than the
            // current worst-survivor ⇒ replace.
            if entry < *worst {
                heap.pop();
                heap.push(entry);
            }
        }
        true
    });

    // `into_sorted_vec` returns ascending-by-Ord, i.e. best-first under
    // our ordering (worst is the heap-max).
    for entry in heap.into_sorted_vec() {
        if !sink(entry.row) {
            return false;
        }
    }
    true
}

struct Entry {
    ord: OrdKey,
    row: Value,
}

/// Heap-ordering key. The `Ord` impl is the canonical row-comparison:
/// for each sort key in declaration order, compare the scalars (with
/// the type-rank fallback for cross-type rows), then flip the result
/// when that key was declared `desc`. Equal-keyed rows tiebreak on
/// insertion order so the surviving K is stable.
struct OrdKey {
    dirs: Vec<SortDir>,
    keys: Vec<Scalar>,
    seq: u64,
}

impl Ord for OrdKey {
    fn cmp(&self, other: &Self) -> Ordering {
        for ((sa, sb), dir) in self.keys.iter().zip(other.keys.iter()).zip(self.dirs.iter()) {
            let ord = sa
                .compare(sb)
                .unwrap_or_else(|| scalar_type_rank(sa).cmp(&scalar_type_rank(sb)));
            if ord != Ordering::Equal {
                return if matches!(dir, SortDir::Desc) { ord.reverse() } else { ord };
            }
        }
        self.seq.cmp(&other.seq)
    }
}

impl PartialOrd for OrdKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for OrdKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for OrdKey {}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ord.cmp(&other.ord)
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.ord == other.ord
    }
}

impl Eq for Entry {}

fn scalar_type_rank(s: &Scalar) -> u8 {
    match s {
        Scalar::Null => 0,
        Scalar::Bool(_) => 1,
        Scalar::Number(_) => 2,
        Scalar::Str(_) => 3,
        Scalar::Container(_) => 4,
    }
}
