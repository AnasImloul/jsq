//! End-to-end tests for the surface query language (SQL-shaped DSL).
//! Each test compiles a surface query against the same kind of fixture
//! the legacy tests used and asserts on the rendered result rows.

use engine::document::Document;
use engine::query;
use engine::query::index::ForeignKeyIndex;
use engine::query::{evaluator, surface};

use std::io::Write;

fn write_tmp(name: &str, content: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    path
}

fn run_surface(doc: &Document, query: &str) -> Vec<String> {
    let ast = surface::compile(query).expect("surface compile ok");
    let out = evaluator::run(doc, &ast, 0, 5000);
    if let Some(err) = out.error {
        panic!("surface eval error: {:?}\nlowered AST: {}", err, ast);
    }
    format_results(doc, out.results)
}

fn format_results(
    doc: &Document,
    rows: Vec<engine::query::evaluator::QueryResult>,
) -> Vec<String> {
    rows.into_iter()
        .map(|r| {
            let mut body = String::new();
            engine::query::evaluator::render::write_value_json(&mut body, doc, &r.value);
            format!("{} → {}", r.path, body)
        })
        .collect()
}

/// Register a foreign-key index keyed by `source` / `key` (in canonical
/// post-lower form). The engine's `Lookup` evaluates against this
/// registry, so joins miss without it.
fn create_index(doc: &Document, source: &str, key: &str) {
    let source_ast = surface::compile_path_only(source).expect("source parse ok");
    let key_ast = surface::compile_path_only(key).expect("key parse ok");
    let canon_src = source_ast.to_string();
    let canon_key = key_ast.to_string();
    let idx = ForeignKeyIndex::build(doc, &source_ast, &key_ast);
    doc.indexes.lock().unwrap().insert(canon_src, canon_key, idx);
}

/// Cube fixture: a handful of series rows joined to a dimensions table.
/// Used by most join / aggregate / partition tests.
fn cube_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_cube_{}.json", test_name),
        r#"{"data":{"kpis":{
            "series":[
                {"id":"s1","dims_id":"d1","granularity":"day","values":{"weight":{"total":100,"adjusted":90}}},
                {"id":"s2","dims_id":"d2","granularity":"day","values":{"weight":{"total":200,"adjusted":195}}},
                {"id":"s3","dims_id":"d3","granularity":"day","values":{"weight":{"total":50,"adjusted":40}}},
                {"id":"s4","dims_id":"d1","granularity":"week","values":{"weight":{"total":700,"adjusted":680}}},
                {"id":"s5","dims_id":"d4","granularity":"day","values":{"weight":{"total":11000,"adjusted":10800}}},
                {"id":"s6","dims_id":"d5","granularity":"day","values":{"weight":{"total":15,"adjusted":15}}},
                {"id":"s7","dims_id":"d6","granularity":"day","values":{"weight":{"total":1000,"adjusted":990}}},
                {"id":"s8","dims_id":"d6","granularity":"week","values":{"weight":{"total":7000,"adjusted":6900}}}
            ],
            "dimensions":[
                {"id":"d1","pay_category":"*","flow":"*","client":"acme","warehouse_id":"wh_07","cargo_type":"*","work_type_id":"*","worker_type":"*","worker_role":"*","worker_level":"*","shift_schedule":"*","shift_template_id":"*"},
                {"id":"d2","pay_category":"*","flow":"*","client":"acme","warehouse_id":"wh_01","cargo_type":"*","work_type_id":"*","worker_type":"*","worker_role":"*","worker_level":"*","shift_schedule":"*","shift_template_id":"*"},
                {"id":"d3","pay_category":"reg","flow":"*","client":"internal_test","warehouse_id":"wh_qa","cargo_type":"frozen","work_type_id":"*","worker_type":"*","worker_role":"*","worker_level":"*","shift_schedule":"*","shift_template_id":"*"},
                {"id":"d4","pay_category":"*","flow":"*","client":"globex","warehouse_id":"wh_12","cargo_type":"frozen","work_type_id":"*","worker_type":"*","worker_role":"*","worker_level":"*","shift_schedule":"*","shift_template_id":"*"},
                {"id":"d5","pay_category":"*","flow":"*","client":"acme","warehouse_id":"wh_sandbox","cargo_type":"*","work_type_id":"*","worker_type":"*","worker_role":"*","worker_level":"*","shift_schedule":"*","shift_template_id":"*"},
                {"id":"d6","pay_category":"*","flow":"*","client":"*","warehouse_id":"*","cargo_type":"*","work_type_id":"*","worker_type":"*","worker_role":"*","worker_level":"*","shift_schedule":"*","shift_template_id":"*"}
            ]
        }}}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    create_index(&doc, ".data.kpis.dimensions[]", ".id");
    doc
}

fn pattern_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_pattern_{}.json", test_name),
        r#"{"items":[
            {"client":"acme_us",     "warehouse":"wh_eu_paris"},
            {"client":"acme_eu",     "warehouse":"wh_eu_berlin"},
            {"client":"acme",        "warehouse":"wh_us_la"},
            {"client":"globex_main", "warehouse":"wh_us_chicago"},
            {"client":"initech",     "warehouse":"wh_apac_tokyo"}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

// ============================================================================
// Source + join + basic where/sum
// ============================================================================

#[test]
fn q1_full_rollup_with_field_set() {
    let doc = cube_doc("q1");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.{
            pay_category, flow, client, warehouse_id, cargo_type,
            work_type_id, worker_type, worker_role, worker_level,
            shift_schedule, shift_template_id,
        } == "*"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    // Only d6 has every rollup dim set to "*". Its series s7 (day,
    // 1000) and s8 (week, 7000) survive.
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2, "{:?}", out);
    let day = out.iter().find(|r| r.starts_with("day → ")).unwrap();
    assert!(day.contains("\"weight\": 1000"), "{}", day);
    let week = out.iter().find(|r| r.starts_with("week → ")).unwrap();
    assert!(week.contains("\"weight\": 7000"), "{}", week);
}

#[test]
fn q2_single_slice_with_and_predicate() {
    let doc = cube_doc("q2");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.warehouse_id == "wh_07" and dim.client == "acme"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2, "{:?}", out);
    let day = out.iter().find(|r| r.starts_with("day → ")).unwrap();
    assert!(day.contains("\"weight\": 100"), "{}", day);
    let week = out.iter().find(|r| r.starts_with("week → ")).unwrap();
    assert!(week.contains("\"weight\": 700"), "{}", week);
}

#[test]
fn q3_in_membership() {
    let doc = cube_doc("q3");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.warehouse_id in ["wh_01", "wh_07", "wh_12"]
          and dim.cargo_type == "frozen"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 1, "{:?}", out);
    assert!(out[0].starts_with("day → "), "{}", out[0]);
    assert!(out[0].contains("\"weight\": 11000"), "{}", out[0]);
}

#[test]
fn q4_ne_and_not_in_with_multi_key() {
    let doc = cube_doc("q4");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.client != "internal_test"
          and dim.warehouse_id not in ["wh_sandbox", "wh_qa"]
        aggregate { weight: sum(s.values.weight.total) } by s.granularity, dim.warehouse_id
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 6, "got {:?}", out);
    let joined: String = out.join("\n");
    for expected_n in ["100", "200", "700", "11000", "1000", "7000"] {
        assert!(
            joined.contains(&format!("\"weight\": {}", expected_n)),
            "expected bucket containing weight={} in {:?}",
            expected_n,
            out
        );
    }
}

#[test]
fn q5_numeric_predicate_no_join() {
    let doc = cube_doc("q5");
    let q = r#"
        from .data.kpis.series[] as s
        where s.values.weight.total > 10000 and s.granularity == "day"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 1, "{:?}", out);
    assert!(out[0].starts_with("day → "), "{}", out[0]);
    assert!(out[0].contains("\"weight\": 11000"), "{}", out[0]);
}

#[test]
fn join_canonical_form_hits_index() {
    let doc = cube_doc("smoke");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.warehouse_id == "wh_07"
        aggregate { n: count() } by s.granularity
    "#;
    let ast = surface::compile(q).expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert!(
        out.error.is_none(),
        "expected no MissingIndex; got {:?} (lowered: {})",
        out.error,
        ast
    );
}

// ============================================================================
// `fields` macro field-set spread + override
// ============================================================================

#[test]
fn fields_macro_spread_matches_inline() {
    let doc = cube_doc("spread");
    let inline = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.{
            pay_category, flow, client, warehouse_id, cargo_type,
            work_type_id, worker_type, worker_role, worker_level,
            shift_schedule, shift_template_id,
        } == "*"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    let spread = r#"
        fields rollup_dims = {
            pay_category, flow, client, warehouse_id, cargo_type,
            work_type_id, worker_type, worker_role, worker_level,
            shift_schedule, shift_template_id,
        }

        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.{...rollup_dims} == "*"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    let inline_out = run_surface(&doc, inline);
    let spread_out = run_surface(&doc, spread);
    assert_eq!(inline_out, spread_out);
    assert!(!spread_out.is_empty());
}

#[test]
fn fields_macro_spread_with_override() {
    let doc = cube_doc("override");
    let spread = r#"
        fields rollup_dims = {
            pay_category, flow, client, warehouse_id, cargo_type,
            work_type_id, worker_type, worker_role, worker_level,
            shift_schedule, shift_template_id,
        }

        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.{...rollup_dims, cargo_type: "frozen"} == "*"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    let inline = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.pay_category == "*"
          and dim.flow == "*"
          and dim.client == "*"
          and dim.warehouse_id == "*"
          and dim.cargo_type == "frozen"
          and dim.work_type_id == "*"
          and dim.worker_type == "*"
          and dim.worker_role == "*"
          and dim.worker_level == "*"
          and dim.shift_schedule == "*"
          and dim.shift_template_id == "*"
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
    "#;
    assert_eq!(run_surface(&doc, spread), run_surface(&doc, inline));
}

// ============================================================================
// select projection
// ============================================================================

#[test]
fn select_projection_emits_synthetic_objects() {
    let doc = cube_doc("select");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        where dim.warehouse_id in ["wh_01", "wh_07"]
        select {
            warehouse: dim.warehouse_id,
            day:       s.granularity,
            weight:    s.values.weight.total,
            client:    dim.client,
        }
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 3, "got rows: {:?}", out);
    for row in &out {
        for field in ["warehouse", "day", "weight", "client"] {
            assert!(
                row.contains(&format!("\"{}\":", field)),
                "row missing field {}: {}",
                field,
                row
            );
        }
    }
    let s1 = out.iter().find(|r| r.contains("\"weight\": 100")).unwrap();
    assert!(s1.contains("\"warehouse\": \"wh_07\""));
    assert!(s1.contains("\"client\": \"acme\""));
}

#[test]
fn select_with_missing_join_emits_null() {
    let doc = cube_doc("select_null");
    let q = r#"
        from .data.kpis.series[] as s
        left join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        select {
            day:       s.granularity,
            warehouse: dim.warehouse_id,
        }
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 8);
    for row in &out {
        assert!(row.contains("\"warehouse\":"), "row: {}", row);
    }
}

// ============================================================================
// pattern operators
// ============================================================================

#[test]
fn pattern_operators_starts_ends_contains_matches() {
    let doc = pattern_doc("patterns");

    let starts = run_surface(
        &doc,
        r#"from .items[] as i where i.client starts_with "acme" select { c: i.client }"#,
    );
    assert_eq!(starts.len(), 3);

    let ends = run_surface(
        &doc,
        r#"from .items[] as i where i.warehouse ends_with "berlin" select { w: i.warehouse }"#,
    );
    assert_eq!(ends.len(), 1);
    assert!(ends[0].contains("\"w\": \"wh_eu_berlin\""));

    let contains = run_surface(
        &doc,
        r#"from .items[] as i where i.warehouse contains "_us_" select { w: i.warehouse }"#,
    );
    assert_eq!(contains.len(), 2);

    let matches = run_surface(
        &doc,
        r#"from .items[] as i where i.client matches "acme_*" select { c: i.client }"#,
    );
    assert_eq!(matches.len(), 2);

    let eu = run_surface(
        &doc,
        r#"from .items[] as i where i.warehouse matches "wh_eu_*" select { w: i.warehouse }"#,
    );
    assert_eq!(eu.len(), 2);

    let two_letter = run_surface(
        &doc,
        r#"from .items[] as i where i.client matches "acme_??" select { c: i.client }"#,
    );
    assert_eq!(two_letter.len(), 2);
}

// ============================================================================
// order by / limit
// ============================================================================

#[test]
fn order_by_desc_with_limit() {
    let doc = cube_doc("order_desc");
    let q = r#"
        from .data.kpis.series[] as s
        select {
            warehouse: s.dims_id,
            weight:    s.values.weight.total,
        }
        order by .weight desc
        limit 3
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 3);
    let weights: Vec<_> = out
        .iter()
        .map(|r| extract_int(r, "weight"))
        .collect();
    assert_eq!(weights, vec!["11000", "7000", "1000"]);
}

#[test]
fn order_by_default_direction_is_ascending() {
    let doc = cube_doc("order_asc");
    let q = r#"
        from .data.kpis.series[] as s
        select { weight: s.values.weight.total }
        order by .weight
        limit 3
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 3);
    let first_weights: Vec<_> = out
        .iter()
        .map(|r| extract_int(r, "weight"))
        .collect();
    assert_eq!(first_weights, vec!["15", "50", "100"]);
}

#[test]
fn order_by_multiple_keys_with_tiebreak() {
    let doc = cube_doc("order_multi");
    let q = r#"
        from .data.kpis.series[] as s
        select {
            g: s.granularity,
            w: s.values.weight.total,
        }
        order by .g asc, .w desc
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 8);
    let g_seq: Vec<_> = out
        .iter()
        .map(|r| extract_string(r, "g"))
        .collect();
    assert_eq!(
        g_seq,
        vec!["day", "day", "day", "day", "day", "day", "week", "week"]
    );
    let w_seq: Vec<_> = out
        .iter()
        .map(|r| extract_int(r, "w"))
        .collect();
    assert_eq!(
        w_seq,
        vec!["11000", "1000", "200", "100", "50", "15", "7000", "700"]
    );
}

fn extract_int(row: &str, field: &str) -> String {
    let needle = format!("\"{}\":", field);
    let frag = row.split(&needle).nth(1).unwrap_or("");
    frag.trim_start_matches(' ')
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()
        .unwrap_or("")
        .to_string()
}

fn extract_string(row: &str, field: &str) -> String {
    let needle = format!("\"{}\":", field);
    let frag = row.split(&needle).nth(1).unwrap_or("");
    frag.trim_start_matches(|c: char| c == ' ' || c == '"')
        .split('"')
        .next()
        .unwrap_or("")
        .to_string()
}

// ============================================================================
// aggregate block (non-partitioned)
// ============================================================================

#[test]
fn aggregate_block_multiple_reducers_by_key() {
    let doc = cube_doc("agg_block");
    let q = r#"
        from .data.kpis.series[] as s
        aggregate {
            total_weight:   sum(s.values.weight.total),
            shipment_count: count(),
            avg_weight:     avg(s.values.weight.total),
            peak:           max(s.values.weight.total),
        } by s.granularity
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2, "{:?}", out);

    let day = out.iter().find(|r| r.starts_with("day → ")).unwrap();
    assert!(day.contains("\"total_weight\": 12365"), "{}", day);
    assert!(day.contains("\"shipment_count\": 6"), "{}", day);
    assert!(day.contains("\"peak\": 11000"), "{}", day);
    assert!(day.contains("\"avg_weight\": 2060."), "{}", day);

    let week = out.iter().find(|r| r.starts_with("week → ")).unwrap();
    assert!(week.contains("\"total_weight\": 7700"), "{}", week);
    assert!(week.contains("\"shipment_count\": 2"), "{}", week);
    assert!(week.contains("\"peak\": 7000"), "{}", week);
    assert!(week.contains("\"avg_weight\": 3850"), "{}", week);
}

#[test]
fn aggregate_block_conditional_reducer_via_where() {
    let doc = cube_doc("agg_cond");
    let q = r#"
        from .data.kpis.series[] as s
        join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
        aggregate {
            frozen_weight: sum(s.values.weight.total) where dim.cargo_type == "frozen",
            total_weight:  sum(s.values.weight.total),
        } by s.granularity
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2);

    let day = out.iter().find(|r| r.starts_with("day → ")).unwrap();
    assert!(day.contains("\"frozen_weight\": 11050"), "{}", day);
    assert!(day.contains("\"total_weight\": 12365"), "{}", day);

    let week = out.iter().find(|r| r.starts_with("week → ")).unwrap();
    assert!(week.contains("\"frozen_weight\": null"), "{}", week);
    assert!(week.contains("\"total_weight\": 7700"), "{}", week);
}

#[test]
fn aggregate_block_then_order_then_limit() {
    let doc = cube_doc("agg_top1");
    let q = r#"
        from .data.kpis.series[] as s
        aggregate { weight: sum(s.values.weight.total) } by s.granularity
        order by .weight desc
        limit 1
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 1);
    let row = &out[0];
    assert!(row.starts_with("day → "), "{}", row);
    assert!(row.contains("\"weight\": 12365"), "{}", row);
}

#[test]
fn aggregate_block_rollup_emits_subtotals_and_grand_total() {
    let path = write_tmp(
        "engine_query_surface_rollup.json",
        r#"{"sales":[
            {"region":"west","product":"a","amount":10},
            {"region":"west","product":"a","amount":5},
            {"region":"west","product":"b","amount":7},
            {"region":"east","product":"a","amount":3},
            {"region":"east","product":"b","amount":9},
            {"region":"east","product":"b","amount":1}
        ]}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let q = r#"
        from .sales[] as s
        aggregate { total: sum(s.amount), n: count() }
        by rollup(s.region, s.product)
    "#;
    let out = run_surface(&doc, q);
    // 4 detail rows (west/a, west/b, east/a, east/b) + 2 region subtotals
    // + 1 grand total.
    assert_eq!(out.len(), 7, "{:#?}", out);

    // Detail level: both key columns carry a value.
    let west_a = out
        .iter()
        .find(|r| r.contains("\"region\": \"west\", \"product\": \"a\""))
        .unwrap();
    assert!(west_a.contains("\"total\": 15"), "{}", west_a);
    assert!(west_a.contains("\"n\": 2"), "{}", west_a);

    // Region subtotal: the trailing `product` key rolls up to null.
    let west_sub = out
        .iter()
        .find(|r| r.contains("\"region\": \"west\", \"product\": null"))
        .unwrap();
    assert!(west_sub.contains("\"total\": 22"), "{}", west_sub);
    assert!(west_sub.contains("\"n\": 3"), "{}", west_sub);

    let east_sub = out
        .iter()
        .find(|r| r.contains("\"region\": \"east\", \"product\": null"))
        .unwrap();
    assert!(east_sub.contains("\"total\": 13"), "{}", east_sub);
    assert!(east_sub.contains("\"n\": 3"), "{}", east_sub);

    // Grand total: every key column is null.
    let grand = out
        .iter()
        .find(|r| r.contains("\"region\": null, \"product\": null"))
        .unwrap();
    assert!(grand.contains("\"total\": 35"), "{}", grand);
    assert!(grand.contains("\"n\": 6"), "{}", grand);
}

// ============================================================================
// grouped aggregate over buckets (was the removed `partition`/`each` form)
// ============================================================================

fn partition_cube_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_partition_{}.json", test_name),
        r#"{"rows":[
            {"cargo_type":"BUP","forecast":120,"baseline":100},
            {"cargo_type":"BUP","forecast":80,"baseline":100},
            {"cargo_type":"LOO","forecast":50,"baseline":40},
            {"cargo_type":"LOO","forecast":60,"baseline":40},
            {"cargo_type":"GEN","forecast":200,"baseline":250}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

// Regression: the use case the removed `partition`/`aggregate each` form
// served — per-bucket derived metrics — is still expressible with a
// grouped `aggregate { ... } by KEY`.
#[test]
fn grouped_aggregate_derives_per_bucket_metrics() {
    let doc = partition_cube_doc("basic");
    let q = r#"
        from .rows[] as r
        let fw = sum(r.forecast),
            bw = sum(r.baseline)
        aggregate {
            pct:    (fw - bw) / bw * 100,
            delta:  fw - bw
        } by r.cargo_type
    "#;
    let out = run_surface(&doc, q);
    // One row per cargo_type bucket.
    // BUP: fw=200, bw=200 → pct=0, delta=0
    // LOO: fw=110, bw=80  → pct=37.5, delta=30
    // GEN: fw=200, bw=250 → pct=-20, delta=-50
    assert_eq!(out.len(), 3, "{:?}", out);
    let joined = out.join("\n");
    assert!(joined.contains("BUP"), "missing BUP: {:?}", out);
    assert!(joined.contains("LOO"), "missing LOO: {:?}", out);
    assert!(joined.contains("GEN"), "missing GEN: {:?}", out);
    assert!(joined.contains("37.5"), "missing loo pct (37.5): {:?}", out);
    assert!(joined.contains("-50"), "missing gen delta (-50): {:?}", out);
}

// ============================================================================
// distinct / collect by
// ============================================================================

#[test]
fn distinct_dedupes_a_stream() {
    let path = write_tmp(
        "engine_query_distinct_strings.json",
        r#"{"users":[
            {"role":"admin"},
            {"role":"user"},
            {"role":"admin"},
            {"role":"guest"},
            {"role":"user"}
        ]}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let rows = run_surface(&doc, "from .users[] as u distinct select { r: u.role }");
    assert_eq!(rows.len(), 3, "{:?}", rows);
}

#[test]
fn collect_by_collects_members_per_bucket() {
    let path = write_tmp(
        "engine_query_collect_by.json",
        r#"{"products":[
            {"id":1,"sku":"A","name":"apple"},
            {"id":2,"sku":"B","name":"banana"},
            {"id":3,"sku":"A","name":"avocado"},
            {"id":4,"sku":"C","name":"cherry"},
            {"id":5,"sku":"A","name":"apricot"},
            {"id":6,"sku":"B","name":"blueberry"}
        ]}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let rows = run_surface(&doc, "from .products[] as p collect by p.sku");
    assert_eq!(rows.len(), 3);
    // Group buckets now render as real JSON arrays of their members
    // — `jsq … | jq .` round-trips cleanly instead of bailing on a
    // `[N items]` placeholder.
    assert!(rows[0].starts_with("A → ["), "{:?}", rows[0]);
    assert!(rows[0].contains("\"name\":\"apple\""), "{:?}", rows[0]);
    assert!(rows[0].contains("\"name\":\"avocado\""), "{:?}", rows[0]);
    assert!(rows[0].contains("\"name\":\"apricot\""), "{:?}", rows[0]);
    assert!(rows[1].starts_with("B → ["), "{:?}", rows[1]);
    assert!(rows[1].contains("\"name\":\"banana\""), "{:?}", rows[1]);
    assert!(rows[1].contains("\"name\":\"blueberry\""), "{:?}", rows[1]);
    assert!(rows[2].starts_with("C → ["), "{:?}", rows[2]);
    assert!(rows[2].contains("\"name\":\"cherry\""), "{:?}", rows[2]);
}

#[test]
fn aggregate_block_no_by_emits_one_named_row() {
    let doc = cube_doc("agg_no_by");
    let out = run_surface(
        &doc,
        "from .data.kpis.series[] as s aggregate { total: sum(s.values.weight.total) }",
    );
    assert_eq!(out, vec!["total → 20065"]);
}

#[test]
fn aggregate_count_without_by_or_arg() {
    let doc = cube_doc("count_no_by");
    let out = run_surface(
        &doc,
        "from .data.kpis.series[] as s aggregate { n: count() }",
    );
    assert_eq!(out, vec!["n → 8"]);
}

#[test]
fn bare_reducer_clause_is_rejected() {
    // The `sum X` / `count` / `count by K` clause-level shorthand was
    // removed — reducer calls now only exist inside an `aggregate { ... }`
    // block. The error message must point users at the block form.
    for q in [
        "from .x[] as r sum r.v",
        "from .x[] as r count",
        "from .x[] as r count by r.k",
        "from .x[] as r avg r.v by r.k",
    ] {
        let err = surface::compile(q).expect_err(q);
        let msg = format!("{:?}", err);
        assert!(msg.contains("aggregate {"), "{}: {}", q, msg);
    }
}

#[test]
fn aggregate_block_without_by_emits_one_row_per_reduction() {
    let doc = cube_doc("agg_block_no_by");
    let q = r#"
        from .data.kpis.series[] as s
        aggregate {
            total: sum(s.values.weight.total),
            rows:  count(),
            peak:  max(s.values.weight.total),
        }
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(
        out,
        vec![
            "total → 20065",
            "rows → 8",
            "peak → 11000",
        ]
    );
}

#[test]
fn aggregate_empty_distinguishes_no_values_from_zero() {
    let doc = cube_doc("agg_null");
    let q = r#"
        from .data.kpis.series[] as s
        where s.granularity == "monthly_unobtainable"
        aggregate {
            total:        sum(s.values.weight.total),
            with_default: sum(s.values.weight.total) ?? 0,
            with_label:   sum(s.values.weight.total) ?? null,
        }
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(
        out,
        vec![
            "total → null",
            "with_default → 0",
            "with_label → null",
        ]
    );
}

#[test]
fn count_with_arg_skips_nulls() {
    let path = write_tmp(
        "engine_query_count_arg.json",
        r#"{"items":[
            {"k":"a","v":1},
            {"k":"a","v":2},
            {"k":"a"},
            {"k":"b","v":4},
            {"k":"b"}
        ]}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let q = r#"
        from .items[] as it
        aggregate {
            rows:    count(),
            with_v:  count(it.v),
            max_v:   max(it.v),
        } by it.k
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2);
    let a = out.iter().find(|r| r.starts_with("a → ")).unwrap();
    assert!(a.contains("\"rows\": 3"));
    assert!(a.contains("\"with_v\": 2"));
    assert!(a.contains("\"max_v\": 2"));
    let b = out.iter().find(|r| r.starts_with("b → ")).unwrap();
    assert!(b.contains("\"rows\": 2"));
    assert!(b.contains("\"with_v\": 1"));
    assert!(b.contains("\"max_v\": 4"));
}

// ============================================================================
// exists / mid-path iteration / recursive descent
// ============================================================================

#[test]
fn exists_distinguishes_missing_from_null() {
    let path = write_tmp(
        "engine_query_exists.json",
        r#"{"rows":[
            {"id":"a", "v": 5},
            {"id":"b", "v": 0},
            {"id":"c", "v": null},
            {"id":"d"}
        ]}"#,
    );
    let doc = Document::open(&path, None).unwrap();

    let exists = run_surface(
        &doc,
        r#"from .rows[] as r where r.v exists select { id: r.id }"#,
    );
    assert_eq!(exists.len(), 3);

    let non_null = run_surface(
        &doc,
        r#"from .rows[] as r where r.v != null select { id: r.id }"#,
    );
    assert_eq!(non_null.len(), 2);

    let both = run_surface(
        &doc,
        r#"from .rows[] as r where r.v exists and r.v != null select { id: r.id }"#,
    );
    assert_eq!(both.len(), 2);
}

#[test]
fn iteration_in_middle_of_path() {
    let path = write_tmp(
        "engine_query_mid_iter.json",
        r#"{"data":{
            "kpi_a": {"series":[{"g":"day"},{"g":"day"},{"g":"week"}]},
            "kpi_b": {"series":[{"g":"day"}]},
            "kpi_c": {"unrelated":1}
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let out = run_surface(
        &doc,
        r#"from .data[].series[] as s where s.g == "day" aggregate { n: count() } by s.g"#,
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("\"n\": 3"), "{}", out[0]);
}

#[test]
fn recursive_descent_finds_all_matches() {
    // Recursive descent emits one array of items at each matching
    // depth; the `from` clause then iterates each array so every leaf
    // item arrives as its own row.
    let path = write_tmp(
        "engine_query_descend.json",
        r#"{"top":{
            "items": [{"n": 1}],
            "child":{"items":[{"n": 2}], "deeper":{"items":[{"n": 3}]}},
            "other":{"unrelated": 99}
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let out = run_surface(&doc, r#"from .top.**.items[] as item select { n: item.n }"#);
    assert_eq!(out.len(), 3, "{:?}", out);
    let nums: std::collections::HashSet<String> = out
        .iter()
        .map(|r| extract_int(r, "n"))
        .collect();
    assert!(nums.contains("1") && nums.contains("2") && nums.contains("3"));
}

fn unnest_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_unnest_{}.json", test_name),
        r#"{"orders":[
            {"id":1,"customer":"acme","items":["widget","gadget","gizmo"]},
            {"id":2,"customer":"globex","items":["sprocket"]},
            {"id":3,"customer":"initech","items":[]},
            {"id":4,"customer":"hooli"}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

#[test]
fn unnest_fans_out_one_row_per_array_element() {
    let doc = unnest_doc("basic");
    let out = run_surface(
        &doc,
        "from .orders[] as o unnest o.items as item \
         select { id: o.id, item: item }",
    );
    // 3 (order 1) + 1 (order 2) = 4. Empty array (order 3) and missing
    // field (order 4) drop their rows — inner semantics.
    assert_eq!(out.len(), 4, "{:?}", out);
    assert!(out.iter().any(|r| r.contains("\"id\": 1") && r.contains("\"item\": \"widget\"")));
    assert!(out.iter().any(|r| r.contains("\"id\": 1") && r.contains("\"item\": \"gizmo\"")));
    assert!(out.iter().any(|r| r.contains("\"id\": 2") && r.contains("\"item\": \"sprocket\"")));
    assert!(!out.iter().any(|r| r.contains("\"id\": 3")), "{:?}", out);
    assert!(!out.iter().any(|r| r.contains("\"id\": 4")), "{:?}", out);
}

#[test]
fn unnest_feeds_downstream_aggregate() {
    let doc = unnest_doc("agg");
    let out = run_surface(
        &doc,
        "from .orders[] as o unnest o.items as item \
         aggregate { n: count() } by o.customer",
    );
    // acme=3, globex=1; initech/hooli contribute no rows.
    assert_eq!(out.len(), 2, "{:?}", out);
    let acme = out.iter().find(|r| r.starts_with("acme → ")).unwrap();
    assert!(acme.contains("\"n\": 3"), "{}", acme);
    let globex = out.iter().find(|r| r.starts_with("globex → ")).unwrap();
    assert!(globex.contains("\"n\": 1"), "{}", globex);
}

// ============================================================================
// correlated subqueries
// ============================================================================

fn subquery_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_subquery_{}.json", test_name),
        r#"{
            "customers":[
                {"id":1,"name":"acme"},
                {"id":2,"name":"globex"},
                {"id":3,"name":"initech"}
            ],
            "orders":[
                {"cust_id":1,"total":50},
                {"cust_id":1,"total":70},
                {"cust_id":2,"total":30}
            ]
        }"#,
    );
    Document::open(&path, None).unwrap()
}

#[test]
fn correlated_exists_keeps_matching_outer_rows() {
    let doc = subquery_doc("exists");
    let out = run_surface(
        &doc,
        "from .customers[] as c \
         where (from .orders[] as o where o.cust_id == c.id) exists \
         select { name: c.name }",
    );
    // acme (id 1) and globex (id 2) have orders; initech (id 3) does not.
    assert_eq!(out.len(), 2, "{:?}", out);
    assert!(out.iter().any(|r| r.contains("\"name\": \"acme\"")), "{:?}", out);
    assert!(out.iter().any(|r| r.contains("\"name\": \"globex\"")), "{:?}", out);
    assert!(!out.iter().any(|r| r.contains("initech")), "{:?}", out);
}

#[test]
fn correlated_not_exists_keeps_unmatched_outer_rows() {
    let doc = subquery_doc("not_exists");
    let out = run_surface(
        &doc,
        "from .customers[] as c \
         where not (from .orders[] as o where o.cust_id == c.id) exists \
         select { name: c.name }",
    );
    // Only initech (id 3) has no orders.
    assert_eq!(out.len(), 1, "{:?}", out);
    assert!(out[0].contains("\"name\": \"initech\""), "{:?}", out);
}

#[test]
fn membership_over_subquery_emissions() {
    let doc = subquery_doc("in");
    let out = run_surface(
        &doc,
        "from .customers[] as c \
         where c.id in (from .orders[].cust_id as o) \
         select { name: c.name }",
    );
    // Customer ids appearing in the orders' cust_id stream: 1 and 2.
    assert_eq!(out.len(), 2, "{:?}", out);
    assert!(out.iter().any(|r| r.contains("\"name\": \"acme\"")), "{:?}", out);
    assert!(out.iter().any(|r| r.contains("\"name\": \"globex\"")), "{:?}", out);
}

#[test]
fn scalar_subquery_in_projection() {
    let doc = subquery_doc("scalar");
    let out = run_surface(
        &doc,
        "from .customers[] as c \
         select { name: c.name, \
                  n: (from .orders[] as o where o.cust_id == c.id aggregate { n: count() }) }",
    );
    assert_eq!(out.len(), 3, "{:?}", out);
    let acme = out.iter().find(|r| r.contains("\"name\": \"acme\"")).unwrap();
    assert!(acme.contains("\"n\": 2"), "{}", acme);
    let globex = out.iter().find(|r| r.contains("\"name\": \"globex\"")).unwrap();
    assert!(globex.contains("\"n\": 1"), "{}", globex);
    let initech = out.iter().find(|r| r.contains("\"name\": \"initech\"")).unwrap();
    assert!(initech.contains("\"n\": 0"), "{}", initech);
}

#[test]
fn subquery_formatter_roundtrips() {
    let canonical = "\
from .customers[] as c
where (from .orders[] as o where o.cust_id == c.id) exists
select {
  name: c.name
}";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

#[test]
fn recursive_descent_with_predicate() {
    let path = write_tmp(
        "engine_query_org.json",
        r#"{"org":{
            "engineering":{"sub":{"employees":[
                {"name":"a","role":"manager","department":"eng"},
                {"name":"b","role":"ic",     "department":"eng"}
            ]}},
            "sales":{"employees":[
                {"name":"c","role":"manager","department":"sales"},
                {"name":"d","role":"manager","department":"sales"}
            ]},
            "ops":{"deeper":{"deeper":{"employees":[
                {"name":"e","role":"ic","department":"ops"}
            ]}}}
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let q = r#"
        from .org.**.employees[] as e
        where e.role == "manager"
        aggregate { n: count() } by e.department
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2);
    let sales = out.iter().find(|r| r.contains("sales")).unwrap();
    assert!(sales.contains("\"n\": 2"), "{}", sales);
    let eng = out.iter().find(|r| r.contains("eng")).unwrap();
    assert!(eng.contains("\"n\": 1"), "{}", eng);
}

#[test]
fn iteration_with_field_set_predicate() {
    let path = write_tmp(
        "engine_query_field_set_iter.json",
        r#"{"regions":{
            "us":   {"warehouses":[
                {"status":"ok",   "region_id":"us"},
                {"status":"warn", "region_id":"us"}
            ]},
            "eu":   {"warehouses":[
                {"status":"ok",   "region_id":"eu"},
                {"status":null,   "region_id":"eu"}
            ]},
            "apac": {"warehouses":[
                {"status":"ok"}
            ]}
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let q = r#"
        from .regions[].warehouses[] as w
        where w.{status, region_id} != null
        aggregate { n: count() } by w.region_id
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2);
    let us = out.iter().find(|r| r.contains("us")).unwrap();
    assert!(us.contains("\"n\": 2"), "{}", us);
    let eu = out.iter().find(|r| r.contains("eu")).unwrap();
    assert!(eu.contains("\"n\": 1"), "{}", eu);
}

// ============================================================================
// is / is not type tests
// ============================================================================

fn mixed_types_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_types_{}.json", test_name),
        r#"{"rows":[
            {"id":"a","v":"hello"},
            {"id":"b","v":42},
            {"id":"c","v":true},
            {"id":"d","v":null},
            {"id":"e","v":[1,2,3]},
            {"id":"f","v":{"x":1}},
            {"id":"g"}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

#[test]
fn type_test_matches_each_native_type() {
    let doc = mixed_types_doc("each");
    for (kind, expected_ids) in &[
        ("string", vec!["a"]),
        ("number", vec!["b"]),
        ("bool",   vec!["c"]),
        ("null",   vec!["d"]),
        ("array",  vec!["e"]),
        ("object", vec!["f"]),
    ] {
        let q = format!(
            r#"from .rows[] as r where r.v is {} select {{ id: r.id }}"#,
            kind
        );
        let out = run_surface(&doc, &q);
        let ids: Vec<String> = out
            .iter()
            .map(|r| extract_string(r, "id"))
            .collect();
        let expected: Vec<String> = expected_ids.iter().map(|s| s.to_string()).collect();
        assert_eq!(ids, expected, "kind={}: {:?}", kind, out);
    }
}

#[test]
fn type_test_negation_excludes_match() {
    let doc = mixed_types_doc("negation");
    let out = run_surface(
        &doc,
        r#"from .rows[] as r where r.v is not string select { id: r.id }"#,
    );
    let ids: Vec<String> = out
        .iter()
        .map(|r| extract_string(r, "id"))
        .collect();
    assert_eq!(ids, vec!["b", "c", "d", "e", "f"]);
}

#[test]
fn type_test_unknown_kind_errors() {
    let res = surface::compile(r#"from .rows[] as r where r.v is widget"#);
    assert!(res.is_err());
}

// ============================================================================
// arithmetic
// ============================================================================

#[test]
fn arithmetic_precedence_and_associativity() {
    let path = write_tmp("engine_arith_lit.json", r#"{"rows":[{"x":7,"y":2,"z":3}]}"#);
    let doc = Document::open(&path, None).unwrap();
    let rows = run_surface(
        &doc,
        r#"from .rows[] as r aggregate {
              add_then_mul: sum(r.x) + sum(r.y) * sum(r.z),
              sub_left:     sum(r.x) - sum(r.y) - sum(r.z),
              parens:       (sum(r.x) - sum(r.y)) * sum(r.z),
              div_then_sub: sum(r.x) / sum(r.y) - sum(r.z),
          }"#,
    );
    assert_eq!(
        rows,
        vec![
            "add_then_mul → 13",
            "sub_left → 2",
            "parens → 15",
            "div_then_sub → 0.5",
        ]
    );
}

#[test]
fn arithmetic_in_where_predicate() {
    let path = write_tmp(
        "engine_arith_where.json",
        r#"{"rows":[
            {"id":"a","x":3,"y":4},
            {"id":"b","x":7,"y":8},
            {"id":"c","x":5,"y":6}
        ]}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let rows = run_surface(&doc, r#"from .rows[] as r where r.x + r.y > 10 aggregate { n: count() }"#);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], "n → 2");
}

#[test]
fn divide_by_zero_with_default() {
    let path = write_tmp("engine_arith_div0.json", r#"{"rows":[{"a":5,"b":0}]}"#);
    let doc = Document::open(&path, None).unwrap();
    let rows = run_surface(
        &doc,
        r#"from .rows[] as r aggregate {
              ratio_raw:    sum(r.a) / sum(r.b),
              ratio_safe:   sum(r.a) / sum(r.b) ?? -1,
          }"#,
    );
    assert_eq!(rows, vec!["ratio_raw → null", "ratio_safe → -1"]);
}

#[test]
fn reducer_deduplication_in_lowerer() {
    let ast = surface::compile(
        r#"from .rows[] as r aggregate { combined: sum(r.x) * 2 + sum(r.x) }"#,
    )
    .expect("compile ok");
    let canon = ast.to_string();
    assert!(canon.contains("$slot(0)"), "expected slot 0: {}", canon);
    assert!(
        !canon.contains("$slot(1)"),
        "reducer should have been deduplicated: {}",
        canon
    );
}

#[test]
fn arithmetic_unary_minus_on_reducer() {
    let path = write_tmp("engine_arith_neg.json", r#"{"rows":[{"x":3},{"x":4}]}"#);
    let doc = Document::open(&path, None).unwrap();
    let rows = run_surface(
        &doc,
        r#"from .rows[] as r aggregate { neg: -sum(r.x) }"#,
    );
    assert_eq!(rows, vec!["neg → -7"]);
}

#[test]
fn reducer_outside_aggregate_block_errors() {
    let res = surface::compile(r#"from .rows[] as r where sum(r.x) > 5"#);
    assert!(res.is_err());
    let msg = format!("{:?}", res.err().unwrap());
    assert!(msg.contains("aggregate"), "{}", msg);
}

// ============================================================================
// alias `let` (post-where alias substitution)
// ============================================================================

#[test]
fn alias_let_substitutes_into_aggregate_item() {
    let doc = partition_cube_doc("alias_only");
    let sugar = r#"
        from .rows[] as r
        let fw = sum(r.forecast)
        aggregate {
            total: fw,
            doubled: fw * 2
        }
    "#;
    let direct = r#"
        from .rows[] as r
        aggregate {
            total: sum(r.forecast),
            doubled: sum(r.forecast) * 2
        }
    "#;
    assert_eq!(run_surface(&doc, sugar), run_surface(&doc, direct));
}

#[test]
fn alias_let_forward_chain() {
    let doc = partition_cube_doc("alias_chain");
    let sugar = r#"
        from .rows[] as r
        let a = sum(r.forecast),
            b = a + 1
        aggregate { result: b }
    "#;
    let direct = r#"
        from .rows[] as r
        aggregate { result: sum(r.forecast) + 1 }
    "#;
    assert_eq!(run_surface(&doc, sugar), run_surface(&doc, direct));
}

#[test]
fn alias_let_shadowing_alias_errors() {
    let res = surface::compile(
        r#"
        from .rows[] as r
        let r = sum(r.x)
        aggregate { x: r }
        "#,
    );
    assert!(res.is_err());
    let msg = format!("{:?}", res.err().unwrap());
    assert!(msg.contains("shadows"), "{}", msg);
}

// ============================================================================
// round() builtin
// ============================================================================

fn numbers_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_round_{}.json", test_name),
        r#"{"rows":[
            {"id":"a","v":1.4},
            {"id":"b","v":2.5},
            {"id":"c","v":3.14159},
            {"id":"d","v":-1.6},
            {"id":"e","v":120.0},
            {"id":"f","v":"not a number"}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

#[test]
fn round_defaults_to_integer() {
    let doc = numbers_doc("integer_default");
    let out = run_surface(
        &doc,
        r#"from .rows[] as r select { id: r.id, n: round(r.v) }"#,
    );
    let combined = out.join("\n");
    for snippet in &["\"n\": 1", "\"n\": 3", "\"n\": -2", "\"n\": 120", "\"n\": null"] {
        assert!(combined.contains(snippet), "missing {} in {:?}", snippet, out);
    }
}

#[test]
fn round_with_precision() {
    let doc = numbers_doc("precision");
    let out = run_surface(
        &doc,
        r#"from .rows[] as r where r.id == "c" select { n: round(r.v, 2) }"#,
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("3.14"));
}

#[test]
fn round_inside_aggregate_hoists_reducer() {
    let doc = numbers_doc("hoist");
    let out = run_surface(
        &doc,
        r#"from .rows[] as r aggregate { total: round(sum(r.v), 1) }"#,
    );
    let combined = out.join("\n");
    assert!(combined.contains("125.4"), "{:?}", out);
}

// ============================================================================
// if() conditional
// ============================================================================

fn if_doc(test_name: &str) -> Document {
    // Mix of booleans, numbers, strings, missing keys, and explicit
    // nulls so the truthiness rule and missing-cond fallback can both
    // be exercised.
    let path = write_tmp(
        &format!("engine_query_if_{}.json", test_name),
        r#"{"rows":[
            {"id":"a","flag":true,  "v":10},
            {"id":"b","flag":false, "v":20},
            {"id":"c","flag":null,  "v":30},
            {"id":"d","flag":0,     "v":40},
            {"id":"e","flag":"yes", "v":50},
            {"id":"f",              "v":60}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

#[test]
fn if_picks_then_when_cond_truthy() {
    // jq truthiness: `0`, `""`, and even a missing-then-emitted value
    // are all truthy. Only `false` and explicit `null` fall through to
    // the else branch. A `cond` that emits nothing also falls through.
    let doc = if_doc("truthy");
    let out = run_surface(
        &doc,
        r#"from .rows[] as r select { id: r.id, pick: if(r.flag, "T", "F") }"#,
    );
    let combined = out.join("\n");
    for (id, expected) in [
        ("a", "T"), // true → T
        ("b", "F"), // false → F
        ("c", "F"), // null → F
        ("d", "T"), // 0 is truthy (jq rule) → T
        ("e", "T"), // "yes" → T
        ("f", "F"), // missing key (no emission) → F
    ] {
        let row = out.iter().find(|r| r.contains(&format!("\"id\": \"{}\"", id))).unwrap();
        assert!(
            row.contains(&format!("\"pick\": \"{}\"", expected)),
            "id={}: expected pick={}, got {}",
            id, expected, row,
        );
        let _ = &combined;
    }
}

#[test]
fn if_branches_can_emit_paths() {
    // The chosen branch's value is emitted as-is — branches don't have
    // to be literals.
    let doc = if_doc("paths");
    let out = run_surface(
        &doc,
        r#"from .rows[] as r select { id: r.id, n: if(r.flag, r.v, -1) }"#,
    );
    let row_a = out.iter().find(|r| r.contains("\"id\": \"a\"")).unwrap();
    assert!(row_a.contains("\"n\": 10"), "{}", row_a);
    let row_b = out.iter().find(|r| r.contains("\"id\": \"b\"")).unwrap();
    assert!(row_b.contains("\"n\": -1"), "{}", row_b);
}

#[test]
fn if_inside_aggregate_hoists_reducers_from_both_branches() {
    // Hoisting must reach both branches. Here the chosen branch
    // depends on a count; the two reductions inside the arms should
    // both be present in the lowered AST as separate slots.
    let doc = if_doc("hoist");
    let ast = surface::compile(
        r#"from .rows[] as r aggregate {
              picked: if(count() > 0, sum(r.v), -1)
          }"#,
    )
    .expect("compile ok");
    let canon = ast.to_string();
    assert!(canon.contains("$slot(0)"), "{}", canon);
    assert!(canon.contains("$slot(1)"), "{}", canon);
    let out = evaluator::run(&doc, &ast, 0, 5000);
    let rows = format_results(&doc, out.results);
    assert_eq!(rows, vec!["picked → 210"]);
}

#[test]
fn if_requires_three_arguments() {
    for q in [
        "from .x[] as r select { v: if(r.flag) }",
        "from .x[] as r select { v: if(r.flag, 1) }",
        "from .x[] as r select { v: if(r.flag, 1, 2, 3) }",
    ] {
        let err = surface::compile(q).expect_err(q);
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("if") || msg.contains("three arguments"),
            "{}: {}",
            q, msg,
        );
    }
}

#[test]
fn if_formatter_roundtrips() {
    let canonical = "\
from .rows[] as r
select {
  v: if(r.flag, r.v, -1)
}";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

// ============================================================================
// formatter idempotence + canonical layouts
// ============================================================================

#[test]
fn formatter_is_idempotent_on_canonical_input() {
    let canonical = "\
from .data.kpis.series[] as s
join .data.kpis.dimensions[] as dim
  on dim.id == s.dims_id
where dim.warehouse_id == \"wh_07\"
and dim.client == \"acme\"
aggregate {
  weight: sum(s.values.weight.total)
} by s.granularity";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

#[test]
fn formatter_normalises_messy_input() {
    let messy = "from .data.kpis.series[] as s join .data.kpis.dimensions[] as dim on dim.id==s.dims_id where dim.warehouse_id==\"wh_07\" and dim.client==\"acme\" aggregate{weight:sum(s.values.weight.total)}by s.granularity";
    let formatted = surface::format(messy).expect("format ok");
    let expected = "\
from .data.kpis.series[] as s
join .data.kpis.dimensions[] as dim
  on dim.id == s.dims_id
where dim.warehouse_id == \"wh_07\"
and dim.client == \"acme\"
aggregate {
  weight: sum(s.values.weight.total)
} by s.granularity";
    assert_eq!(formatted, expected);
}

#[test]
fn formatter_round_trips_collect_by() {
    let canonical = "\
from .products[] as p
collect by p.sku";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

#[test]
fn formatter_round_trips_fields_macro() {
    let canonical = "\
fields core = {
  name,
  sku
}

from .products[] as p
where p.{...core} != null";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

#[test]
fn formatter_round_trips_rollup() {
    let canonical = "\
from .sales[] as s
aggregate {
  total: sum(s.amount)
} by rollup(s.region, s.product)";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

#[test]
fn hash_is_not_a_comment() {
    let err = surface::compile("# hello\nfrom .foo as f").expect_err("lex error");
    assert!(err.message.contains('#'));
}

// ============================================================================
// FFI / scanned-rows / lookup-call counters
// ============================================================================

#[test]
fn query_compile_runs_surface_end_to_end() {
    let doc = cube_doc("compile");
    let ast = query::compile(
        r#"from .data.kpis.series[] as s where s.granularity == "week" aggregate { weight: sum(s.values.weight.total) } by s.granularity"#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert!(out.error.is_none());
    let formatted = format_results(&doc, out.results);
    assert_eq!(formatted.len(), 1);
    assert!(formatted[0].starts_with("week → "), "{}", formatted[0]);
    assert!(formatted[0].contains("\"weight\": 7700"), "{}", formatted[0]);
}

#[test]
fn scanned_rows_counts_source_emissions() {
    let doc = cube_doc("scanned");
    let ast = query::compile("from .data.kpis.series[] as s aggregate { n: count() }")
        .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.scanned_rows, 8);

    let ast = query::compile(
        r#"from .data.kpis.series[] as s where s.granularity == "week" aggregate { n: count() }"#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.scanned_rows, 8);
    assert_eq!(out.results.len(), 1);
}

#[test]
fn join_runs_one_lookup_per_row() {
    let doc = cube_doc("lookup_calls");

    let ast = query::compile(
        "from .data.kpis.series[] as s aggregate { n: count() } by s.granularity",
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.lookup_calls, 0);

    let ast = query::compile(
        r#"
            from .data.kpis.series[] as s
            join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
            select { wh: dim.warehouse_id }
        "#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.lookup_calls, 8);

    let ast = query::compile(
        r#"
            from .data.kpis.series[] as s
            join .data.kpis.dimensions[] as dim on dim.id == s.dims_id
            where dim.cargo_type == "*" and dim.client == "*"
            aggregate { n: count() } by s.granularity
        "#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.lookup_calls, 8);
}

// ============================================================================
// parse / lower error surfacing
// ============================================================================

#[test]
fn query_must_start_with_from() {
    let res = surface::compile(".rows[] aggregate { n: count() }");
    assert!(res.is_err());
    let msg = format!("{:?}", res.err().unwrap());
    assert!(msg.contains("from"), "{}", msg);
}

#[test]
fn source_alias_is_mandatory() {
    let res = surface::compile("from .rows aggregate { n: count() }");
    assert!(res.is_err());
    let msg = format!("{:?}", res.err().unwrap());
    assert!(msg.contains("as"), "{}", msg);
}

#[test]
fn join_alias_is_mandatory() {
    let res = surface::compile(
        "from .rows[] as r join .other on .id == r.id aggregate { n: count() }",
    );
    assert!(res.is_err());
}

#[test]
fn join_on_clause_is_mandatory() {
    let res = surface::compile(
        "from .rows[] as r join .other[] as o aggregate { n: count() }",
    );
    assert!(res.is_err());
    let msg = format!("{:?}", res.err().unwrap());
    assert!(msg.contains("on"), "{}", msg);
}

#[test]
fn join_on_predicate_must_split_aliases() {
    // Both sides reference the new alias `o` — invalid.
    let res = surface::compile(
        r#"
        from .rows[] as r
        join .other[] as o on o.x == o.y
        aggregate { n: count() }
        "#,
    );
    assert!(res.is_err());
}

#[test]
fn alias_cannot_be_a_reserved_keyword() {
    let res = surface::compile("from .rows[] as where aggregate { n: count() }");
    assert!(res.is_err());
}

#[test]
fn undefined_alias_in_path_errors() {
    let res = surface::compile(
        r#"from .rows[] as r where x.foo == 1 aggregate { n: count() }"#,
    );
    assert!(res.is_err());
    let msg = format!("{:?}", res.err().unwrap());
    assert!(msg.contains("undefined alias") || msg.contains("`x`"), "{}", msg);
}
