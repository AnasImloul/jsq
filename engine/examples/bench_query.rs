// Run a query against a large file and time it.
// Usage: cargo run --release --example bench_query -- <path.json> <query> [index_dir]

use engine::document::Document;
use engine::query::{self, evaluator};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench_query <path.json> <query> [index_dir]");
    let query = args.next().expect("missing query");
    let index_dir = args.next();
    let dir_ref = index_dir.as_deref().map(std::path::Path::new);

    let open_start = Instant::now();
    let doc = Document::open(std::path::Path::new(&path), dir_ref).expect("open");
    let open_elapsed = open_start.elapsed();

    let ast = query::compile(&query).expect("parse");

    let q_start = Instant::now();
    let out = evaluator::run(&doc, &ast, 0, 5000);
    let q_elapsed = q_start.elapsed();

    let from = if doc.loaded_from_sidecar() { "sidecar" } else { "parsed" };
    println!(
        "open={:.3}s ({})  query={:.3}s  results={}  hit_limit={}  query='{}'",
        open_elapsed.as_secs_f64(),
        from,
        q_elapsed.as_secs_f64(),
        out.results.len(),
        out.hit_limit,
        query,
    );
    if let Some(first) = out.results.first() {
        println!("  first: {} → {}", first.path, first.preview);
    }
    if let Some(last) = out.results.last() {
        println!("  last:  {} → {}", last.path, last.preview);
    }
}
