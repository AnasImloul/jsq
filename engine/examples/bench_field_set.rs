// Benchmark for the field-set rollup query — measures whether the
// `Ast::FieldSetEquals` fusion actually pays off at scale.
//
// Builds a synthetic cube: N series rows, K dimensions. Roughly 60% of
// dimensions are "all-stars" (eligible for the rollup) and the rest
// have at least one non-star field. Runs the Q1 query and prints
// elapsed + scanned + lookups.
//
// Usage:
//   cargo run --release --example bench_field_set -- [N_series] [K_dims]
// Defaults: 50_000 series, 5_000 dimensions.

use engine::document::Document;
use engine::query::index::ForeignKeyIndex;
use engine::query::{evaluator, surface};

use std::io::Write;
use std::time::Instant;

const ROLLUP_FIELDS: &[&str] = &[
    "pay_category",
    "flow",
    "client",
    "warehouse_id",
    "cargo_type",
    "work_type_id",
    "worker_type",
    "worker_role",
    "worker_level",
    "shift_schedule",
    "shift_template_id",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let n_series: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let n_dims: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5_000);

    let path = std::env::temp_dir().join("engine_bench_cube.json");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        write_fixture(&mut f, n_series, n_dims).unwrap();
    }

    let doc = Document::open(&path, None).expect("open cube");

    // Build the foreign-key index that the with-binding relies on.
    let source_ast = surface::compile(".data.kpis.dimensions[]").unwrap();
    let key_ast = surface::compile(".id").unwrap();
    let canon_src = source_ast.to_string();
    let canon_key = key_ast.to_string();
    let idx = ForeignKeyIndex::build(&doc, &source_ast, &key_ast);
    doc.indexes.lock().unwrap().insert(canon_src, canon_key, idx);

    let queries = [
        (
            "rollup-fieldset",
            r#"
                .data.kpis.series[]
                  with dim = lookup(.data.kpis.dimensions[]; .dims_id == .id)
                  where dim.{
                      pay_category, flow, client, warehouse_id, cargo_type,
                      work_type_id, worker_type, worker_role, worker_level,
                      shift_schedule, shift_template_id,
                  } == "*"
                  sum .values.weight.total by .granularity
            "#,
        ),
        (
            "projection-with-dim-fields",
            // Per-row dim.X accesses + synthetic object emission.
            r#"
                .data.kpis.series[]
                  with dim = lookup(.data.kpis.dimensions[]; .dims_id == .id)
                  select {
                      wh: dim.warehouse_id,
                      client: dim.client,
                      cargo: dim.cargo_type,
                  }
            "#,
        ),
    ];

    println!(
        "n_series={n_series}  n_dims={n_dims}",
        n_series = n_series,
        n_dims = n_dims
    );
    for (label, q) in queries.iter() {
        let ast = surface::compile(q).expect("compile");
        let mut best_ns: u128 = u128::MAX;
        let mut last_out: Option<evaluator::EvalOutput> = None;
        for _ in 0..5 {
            let start = Instant::now();
            let out = evaluator::run(&doc, &ast, 0, n_series + 1);
            let elapsed = start.elapsed().as_nanos();
            if elapsed < best_ns {
                best_ns = elapsed;
            }
            last_out = Some(out);
        }
        let out = last_out.unwrap();
        let elapsed_ms = best_ns as f64 / 1_000_000.0;
        println!(
            "[{label:>28}]  scanned={}  lookups={}  output={}  best={:.2} ms  per-row={:.2} µs",
            out.scanned_rows,
            out.lookup_calls,
            out.results.len(),
            elapsed_ms,
            (best_ns as f64 / 1_000.0) / n_series as f64,
            label = label,
        );
    }
}

fn write_fixture(f: &mut std::fs::File, n_series: usize, n_dims: usize) -> std::io::Result<()> {
    f.write_all(b"{\"data\":{\"kpis\":{\"series\":[")?;
    for i in 0..n_series {
        if i > 0 {
            f.write_all(b",")?;
        }
        let dim_idx = i % n_dims;
        let granularity = if i % 7 == 0 { "week" } else { "day" };
        let weight = 100 + (i % 5000) as u64;
        let adj = weight - 5;
        write!(
            f,
            r#"{{"id":"s{i}","dims_id":"d{dim}","granularity":"{g}","values":{{"weight":{{"total":{w},"adjusted":{a}}}}}}}"#,
            i = i,
            dim = dim_idx,
            g = granularity,
            w = weight,
            a = adj
        )?;
    }
    f.write_all(b"],\"dimensions\":[")?;
    for i in 0..n_dims {
        if i > 0 {
            f.write_all(b",")?;
        }
        // 60% of dimensions are full rollups (all "*"). The rest have
        // a deterministic non-star value somewhere — gives the where
        // predicate something to filter.
        let is_rollup = i % 5 < 3;
        write!(f, "{{\"id\":\"d{}\"", i)?;
        for (k, &field) in ROLLUP_FIELDS.iter().enumerate() {
            // Spread the non-star deterministically across fields so
            // the short-circuit position varies.
            let value = if is_rollup || k != (i % ROLLUP_FIELDS.len()) {
                "*"
            } else {
                "non-rollup"
            };
            write!(f, ",\"{}\":\"{}\"", field, value)?;
        }
        f.write_all(b"}")?;
    }
    f.write_all(b"]}}}")?;
    Ok(())
}
