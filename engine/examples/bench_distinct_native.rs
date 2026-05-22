// "Hand-rolled native Rust" baseline for `.events[].type distinct`. Walks
// the engine's NodeRecord index directly, with no AST / walker indirection.
// Sets the floor: anything achievable by a generic query engine has to
// pay at least this much per row.
//
// Usage: cargo run --release --example bench_distinct_native

use engine::document::{Document, NodeKind};

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

fn main() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../big.json");
    if !path.exists() {
        eprintln!("big.json not found at {}", path.display());
        std::process::exit(1);
    }

    eprintln!("opening {} ...", path.display());
    let t = Instant::now();
    let doc = Document::open(&path, None).expect("open big.json");
    eprintln!("opened in {:.2} s", t.elapsed().as_secs_f64());

    // Locate `.events` once, outside the timing loop — same as the engine
    // does (the path prefix is part of the AST, not per-row work).
    let root = 0u32;
    let events_array = find_child_by_key(&doc, root, b"events")
        .expect(".events not found");
    assert_eq!(doc.node_kind(events_array), NodeKind::Array);

    // ---------------------------------------------------------------
    // Variant A — match what the engine does today: linear scan of each
    // event's child list to find the `type` field. Uses the same
    // `FxHashSet<&[u8]>` strategy the optimised engine path now uses.
    // ---------------------------------------------------------------
    let a = bench("naive-linear-scan", 5, || {
        let mut seen: HashSet<&[u8], rustc_hash::FxBuildHasher> =
            HashSet::default();
        let mut count: u64 = 0;
        let mut child = doc.first_skippable_child(events_array);
        while child != u32::MAX {
            count += 1;
            if let Some(type_id) = find_child_by_key(&doc, child, b"type") {
                if let Some(bytes) = doc.value_bytes(type_id) {
                    seen.insert(bytes);
                }
            }
            child = doc.next_skippable_sibling(child);
        }
        (count, seen.len())
    });

    // ---------------------------------------------------------------
    // Variant B — same shape, but assume "type" is always the second
    // field (it is, in big.json). The fastest a schema-aware fast path
    // could go: skip the key compare entirely.
    // ---------------------------------------------------------------
    let b = bench("schema-fast-path", 5, || {
        let mut seen: HashSet<&[u8], rustc_hash::FxBuildHasher> =
            HashSet::default();
        let mut count: u64 = 0;
        let mut child = doc.first_skippable_child(events_array);
        while child != u32::MAX {
            count += 1;
            // .type is the 2nd field of every event in big.json.
            let first = doc.first_skippable_child(child);
            if first != u32::MAX {
                let second = doc.next_skippable_sibling(first);
                if second != u32::MAX {
                    if let Some(bytes) = doc.value_bytes(second) {
                        seen.insert(bytes);
                    }
                }
            }
            child = doc.next_skippable_sibling(child);
        }
        (count, seen.len())
    });

    println!();
    println!("           target = 2× of naive-linear-scan = {:.1} ms", a.1 * 2.0);
    println!("       engine now = ~143 ms (from bench_distinct_real)");
    println!("                    {:.2}× of naive  ({:.0}% of target)",
        143.0 / a.1, (143.0 / (a.1 * 2.0)) * 100.0);
    let _ = b;
}

fn bench<R, F: FnMut() -> R>(label: &str, iters: u32, mut f: F) -> (R, f64) {
    let mut best_ns: u128 = u128::MAX;
    let mut last: Option<R> = None;
    for _ in 0..iters {
        let start = Instant::now();
        last = Some(f());
        let elapsed = start.elapsed().as_nanos();
        if elapsed < best_ns {
            best_ns = elapsed;
        }
    }
    let ms = best_ns as f64 / 1_000_000.0;
    println!("[{label:>20}]  best={:.2} ms", ms, label = label);
    (last.unwrap(), ms)
}

fn find_child_by_key(doc: &Document, parent: u32, key: &[u8]) -> Option<u32> {
    let mut child = doc.first_skippable_child(parent);
    while child != u32::MAX {
        if doc.key_bytes(child) == Some(key) {
            return Some(child);
        }
        child = doc.next_skippable_sibling(child);
    }
    None
}
