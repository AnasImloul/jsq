// Like `bench`, but does NOT touch every record after open. Reports the
// peak RSS attributable to opening alone (vs walking the index).
// Usage: cargo run --release --example bench_open -- <path.json> [index_dir]

use engine::document::Document;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench_open <path.json> [index_dir]");
    let index_dir = args.next();
    let dir_ref = index_dir.as_deref().map(std::path::Path::new);

    let start = Instant::now();
    let mut doc = Document::open(std::path::Path::new(&path), dir_ref).expect("open");
    let elapsed = start.elapsed();
    // Wait so the sidecar lands on disk before we exit — otherwise the
    // background finaliser races with process termination and the next
    // run sees no cache.
    doc.wait_for_sidecar();
    let file_mb = doc.source_mmap.len() as f64 / (1024.0 * 1024.0);
    let nodes = doc.records().len();
    let from = if doc.loaded_from_sidecar() { "sidecar" } else { "parsed" };

    // Touch only root + a handful of nodes — typical browse working set.
    let mut sum: u64 = 0;
    let root = doc.records()[0];
    sum = sum.wrapping_add(root.offset);
    let parent_end = root.subtree_size; // root id is 0
    let mut cur = if root.subtree_size > 1 { 1u32 } else { u32::MAX };
    let mut visited = 0;
    while cur != u32::MAX && visited < 50 {
        let r = &doc.records()[cur as usize];
        sum = sum.wrapping_add(r.offset);
        let next = cur + r.subtree_size;
        cur = if next < parent_end { next } else { u32::MAX };
        visited += 1;
    }

    println!(
        "{}  file={:.1}MB  nodes={}  open={:.3}s  ({})  visited={}",
        path,
        file_mb,
        nodes,
        elapsed.as_secs_f64(),
        from,
        visited,
    );
    eprintln!("(checksum {})", sum);
}
