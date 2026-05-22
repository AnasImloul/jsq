// Times `.events[].type distinct` against the project's real big.json
// in two phases — cold (no sidecar, full parse) and warm (sidecar
// present, fast load) — and reports each segment separately so we
// can see where the user-visible "first run vs. subsequent runs"
// gap is actually being spent.
//
// Usage: cargo run --release --example bench_distinct_real [path.json]

use engine::document::Document;
use engine::query::{evaluator, surface};

use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../big.json"));
    if !path.exists() {
        eprintln!("file not found: {}", path.display());
        std::process::exit(1);
    }
    let size_gb = std::fs::metadata(&path).unwrap().len() as f64 / (1024.0 * 1024.0 * 1024.0);
    eprintln!("file: {} ({:.2} GiB)", path.display(), size_gb);

    let sidecar_dir = std::env::temp_dir().join("bigjson-bench-distinct");
    let _ = std::fs::remove_dir_all(&sidecar_dir);
    std::fs::create_dir_all(&sidecar_dir).unwrap();

    let queries = [
        ("source-only        ", ".events[]"),
        ("type-only          ", ".events[].type"),
        ("type-distinct      ", ".events[].type distinct"),
        ("type-distinct-count", ".events[].type distinct count"),
    ];

    println!("\n=== COLD: no sidecar, full parse ===");
    let t = Instant::now();
    let doc = Document::open(&path, Some(&sidecar_dir)).expect("open");
    let cold_load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let from = if doc.loaded_from_sidecar() { "sidecar" } else { "parsed" };
    println!("  load: {:>7.0} ms  ({})", cold_load_ms, from);

    for (label, q) in queries.iter() {
        let ast = surface::compile(q).expect("compile");
        let t = Instant::now();
        let out = evaluator::run(&doc, &ast, 0, usize::MAX);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  query [{label}]: {:>7.0} ms  scanned={}  output={}",
            ms,
            out.scanned_rows,
            out.results.len(),
            label = label,
        );
    }
    drop(doc);
    // Wait for sidecar finalisation thread.
    std::thread::sleep(std::time::Duration::from_millis(750));

    println!("\n=== WARM (sidecar present) — type-distinct as the FIRST query ===");
    println!("(Mirrors the UI flow: parse already done, user opens result tab,");
    println!(" runs the distinct query for the first time on a cold OS page cache.)");
    let t = Instant::now();
    let doc = Document::open(&path, Some(&sidecar_dir)).expect("open");
    let warm_load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let from = if doc.loaded_from_sidecar() { "sidecar" } else { "parsed" };
    println!("  load: {:>7.0} ms  ({})", warm_load_ms, from);

    // Run type-distinct alone first (no warming queries beforehand) — the
    // OS page cache is in whatever state the OS left it after the parse.
    let ast_distinct = surface::compile(".events[].type distinct").expect("compile");
    let t = Instant::now();
    let out = evaluator::run(&doc, &ast_distinct, 0, usize::MAX);
    let first_run_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "  type-distinct (1st run, sources cold): {:>7.0} ms  scanned={}  output={}",
        first_run_ms, out.scanned_rows, out.results.len(),
    );
    // Subsequent runs hit warm OS page cache.
    let mut subsequent: Vec<f64> = Vec::with_capacity(4);
    for _ in 0..4 {
        let t = Instant::now();
        let _ = evaluator::run(&doc, &ast_distinct, 0, usize::MAX);
        subsequent.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let steady = subsequent.iter().cloned().fold(f64::MAX, f64::min);
    println!("  type-distinct (steady-state, OS-warm): {:>7.0} ms", steady);

    println!("\n  for the rest of the queries, OS cache is now warm:");
    for (label, q) in queries.iter() {
        let ast = surface::compile(q).expect("compile");
        let mut times: Vec<f64> = Vec::with_capacity(5);
        let mut scanned = 0u64;
        let mut output = 0usize;
        for _ in 0..5 {
            let t = Instant::now();
            let out = evaluator::run(&doc, &ast, 0, usize::MAX);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            scanned = out.scanned_rows;
            output = out.results.len();
        }
        let first = times[0];
        let last_min = times[1..].iter().cloned().fold(f64::MAX, f64::min);
        println!(
            "    [{label}]: first={:>5.0} ms  steady={:>5.0} ms  scanned={}  output={}",
            first, last_min, scanned, output,
            label = label,
        );
    }

    println!("\n=== summary ===");
    println!("  cold load (parse) = {:>6.0} ms", cold_load_ms);
    println!("  warm load (sidecar) = {:>4.0} ms", warm_load_ms);
    println!("  parse-only cost ≈   {:>6.0} ms  (cold − warm)", cold_load_ms - warm_load_ms);
}
