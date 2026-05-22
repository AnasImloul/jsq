// Quick benchmark harness.
// Usage: cargo run --release --example bench -- <path.json> [index_dir]

use engine::document::Document;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <path.json> [index_dir]");
    let index_dir = args.next();
    let dir_ref = index_dir.as_deref().map(std::path::Path::new);

    let start = Instant::now();
    let mut doc = Document::open(std::path::Path::new(&path), dir_ref).expect("open");
    let elapsed = start.elapsed();
    let file_size_mb = doc.source_mmap.len() as f64 / (1024.0 * 1024.0);
    let nodes = doc.records().len();
    let keys_kb = doc.keys().len() as f64 / 1024.0;
    let from_sidecar = if doc.loaded_from_sidecar() { "sidecar" } else { "parsed" };
    println!(
        "{}  file={:.1}MB  nodes={}  keys-pool={:.1}KB  open={:.3}s ({})  ({:.1} MB/s)",
        path,
        file_size_mb,
        nodes,
        keys_kb,
        elapsed.as_secs_f64(),
        from_sidecar,
        file_size_mb / elapsed.as_secs_f64(),
    );
    // Touch all records so OS pages them in
    let mut sum: u64 = 0;
    for r in doc.records() {
        sum = sum.wrapping_add(r.offset);
    }
    eprintln!("(touched all {} records, checksum {})", nodes, sum);

    // CLI tools die when the process exits — wait for the background
    // sidecar writer so the cache is on disk for the next invocation.
    let wait_start = Instant::now();
    doc.wait_for_sidecar();
    let wait_elapsed = wait_start.elapsed();
    if wait_elapsed.as_millis() > 50 {
        eprintln!("(sidecar finalized after {:.1}s)", wait_elapsed.as_secs_f64());
    }
}
