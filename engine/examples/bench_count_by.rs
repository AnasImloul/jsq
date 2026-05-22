// Measures `count by .X` and `aggregate { events: count } by .X order by ...`
// on a fixture sized to mirror big.json's events array. Numeric group key —
// the case that benefits most from the ScalarKey-keyed bucket map.
//
// Usage: cargo run --release --example bench_count_by [n_events] [n_users]
// Defaults: 1_200_000 events / 877_000 users — matching big.json's ratios.

use engine::document::Document;
use engine::query::{evaluator, surface};

use std::io::Write;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let n_events: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1_200_000);
    let n_users: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(877_000);

    let path = std::env::temp_dir().join("engine_bench_countby.json");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        write_fixture(&mut f, n_events, n_users).unwrap();
    }

    let doc = Document::open(&path, None).expect("open events");

    let queries = [
        (
            "count-by-user_id",
            ".events[] count by .user_id",
        ),
        (
            "agg-count-by-user_id",
            ".events[] aggregate { events: count } by .user_id",
        ),
        (
            "agg-count-top20",
            ".events[] aggregate { events: count } by .user_id order by events desc limit 20",
        ),
        (
            "count-by-type",
            ".events[] count by .type",
        ),
        (
            "agg-count-by-type",
            ".events[] aggregate { events: count } by .type",
        ),
        (
            "count-ok-by-type",
            ".events[] where .ok aggregate { events: count } by .type",
        ),
    ];

    println!("n_events={n_events}  n_users={n_users}", n_events = n_events, n_users = n_users);
    for (label, q) in queries.iter() {
        let ast = surface::compile(q).expect("compile");
        let mut best_ns: u128 = u128::MAX;
        let mut last_out: Option<evaluator::EvalOutput> = None;
        for _ in 0..5 {
            let start = Instant::now();
            let out = evaluator::run(&doc, &ast, 0, n_events + 1);
            let elapsed = start.elapsed().as_nanos();
            if elapsed < best_ns {
                best_ns = elapsed;
            }
            last_out = Some(out);
        }
        let out = last_out.unwrap();
        let elapsed_ms = best_ns as f64 / 1_000_000.0;
        println!(
            "[{label:>22}]  scanned={}  output={}  best={:.2} ms  per-row={:.2} µs",
            out.scanned_rows,
            out.results.len(),
            elapsed_ms,
            (best_ns as f64 / 1_000.0) / n_events as f64,
            label = label,
        );
    }
}

/// Tiny xorshift — good enough to scatter user_ids without pulling in
/// a `rand` dependency just for this benchmark.
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn write_fixture(
    f: &mut std::fs::File,
    n_events: usize,
    n_users: usize,
) -> std::io::Result<()> {
    let mut state: u64 = 0xB16_5500_DEAD_BEEF;
    let types = ["click", "view", "purchase", "scroll", "hover"];
    f.write_all(b"{\"events\":[")?;
    for i in 0..n_events {
        if i > 0 {
            f.write_all(b",")?;
        }
        let user_id = (next_rand(&mut state) as usize) % n_users.max(1);
        let ok = next_rand(&mut state) % 100 < 95;
        let latency = next_rand(&mut state) % 200;
        let typ = types[(next_rand(&mut state) as usize) % types.len()];
        write!(
            f,
            r#"{{"id":{i},"user_id":{u},"ok":{ok},"latency_ms":{l},"type":"{t}"}}"#,
            i = i,
            u = user_id,
            ok = ok,
            l = latency,
            t = typ,
        )?;
    }
    f.write_all(b"]}")?;
    Ok(())
}
