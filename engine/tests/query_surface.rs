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

/// Library fixture: a handful of loan rows joined to a books table.
/// Used by most join / aggregate / grouped-metric tests.
fn library_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_library_{}.json", test_name),
        r#"{"catalog":{
            "loans":[
                {"id":"l1","book_id":"b1","branch":"east","days":{"borrowed":100,"renewed":90}},
                {"id":"l2","book_id":"b2","branch":"east","days":{"borrowed":200,"renewed":195}},
                {"id":"l3","book_id":"b3","branch":"east","days":{"borrowed":50,"renewed":40}},
                {"id":"l4","book_id":"b1","branch":"west","days":{"borrowed":700,"renewed":680}},
                {"id":"l5","book_id":"b4","branch":"east","days":{"borrowed":11000,"renewed":10800}},
                {"id":"l6","book_id":"b5","branch":"east","days":{"borrowed":15,"renewed":15}},
                {"id":"l7","book_id":"b6","branch":"east","days":{"borrowed":1000,"renewed":990}},
                {"id":"l8","book_id":"b6","branch":"west","days":{"borrowed":7000,"renewed":6900}}
            ],
            "books":[
                {"id":"b1","author":"rowling","shelf":"A1","genre":"fiction","available":true,"featured":true,"reservable":false},
                {"id":"b2","author":"rowling","shelf":"A2","genre":"fiction","available":true,"featured":false,"reservable":true},
                {"id":"b3","author":"orwell","shelf":"Q1","genre":"mystery","available":false,"featured":true,"reservable":true},
                {"id":"b4","author":"asimov","shelf":"Z9","genre":"mystery","available":true,"featured":false,"reservable":true},
                {"id":"b5","author":"rowling","shelf":"X0","genre":"fiction","available":true,"featured":true,"reservable":false},
                {"id":"b6","author":"clarke","shelf":"M3","genre":"fiction","available":true,"featured":true,"reservable":true}
            ]
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    create_index(&doc, ".catalog.books[]", ".id");
    doc
}

fn pattern_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_pattern_{}.json", test_name),
        r#"{"items":[
            {"client":"acme_us",     "location":"loc_eu_paris"},
            {"client":"acme_eu",     "location":"loc_eu_berlin"},
            {"client":"acme",        "location":"loc_us_la"},
            {"client":"globex_main", "location":"loc_us_chicago"},
            {"client":"initech",     "location":"loc_apac_tokyo"}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

// ============================================================================
// Source + join + basic where/sum
// ============================================================================

#[test]
fn q1_full_rollup_with_field_set() {
    let doc = library_doc("q1");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.{
            available, featured, reservable,
        } == true
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    // Only b6 has every flag set to true. Its loans l7 (east, 1000)
    // and l8 (west, 7000) survive.
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2, "{:?}", out);
    let east = out.iter().find(|r| r.starts_with("east → ")).unwrap();
    assert!(east.contains("\"total\": 1000"), "{}", east);
    let west = out.iter().find(|r| r.starts_with("west → ")).unwrap();
    assert!(west.contains("\"total\": 7000"), "{}", west);
}

#[test]
fn q2_single_slice_with_and_predicate() {
    let doc = library_doc("q2");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.shelf == "A1" and b.author == "rowling"
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2, "{:?}", out);
    let east = out.iter().find(|r| r.starts_with("east → ")).unwrap();
    assert!(east.contains("\"total\": 100"), "{}", east);
    let west = out.iter().find(|r| r.starts_with("west → ")).unwrap();
    assert!(west.contains("\"total\": 700"), "{}", west);
}

#[test]
fn q3_in_membership() {
    let doc = library_doc("q3");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.shelf in ["A2", "A1", "Z9"]
          and b.genre == "mystery"
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 1, "{:?}", out);
    assert!(out[0].starts_with("east → "), "{}", out[0]);
    assert!(out[0].contains("\"total\": 11000"), "{}", out[0]);
}

#[test]
fn q4_ne_and_not_in_with_multi_key() {
    let doc = library_doc("q4");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.author != "orwell"
          and b.shelf not in ["X0", "Q1"]
        aggregate { total: sum(s.days.borrowed) } by s.branch, b.shelf
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 6, "got {:?}", out);
    let joined: String = out.join("\n");
    for expected_n in ["100", "200", "700", "11000", "1000", "7000"] {
        assert!(
            joined.contains(&format!("\"total\": {}", expected_n)),
            "expected bucket containing total={} in {:?}",
            expected_n,
            out
        );
    }
}

#[test]
fn q5_numeric_predicate_no_join() {
    let doc = library_doc("q5");
    let q = r#"
        from .catalog.loans[] as s
        where s.days.borrowed > 10000 and s.branch == "east"
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 1, "{:?}", out);
    assert!(out[0].starts_with("east → "), "{}", out[0]);
    assert!(out[0].contains("\"total\": 11000"), "{}", out[0]);
}

#[test]
fn join_canonical_form_hits_index() {
    let doc = library_doc("smoke");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.author == "rowling"
        aggregate { n: count() } by s.branch
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
    let doc = library_doc("spread");
    let inline = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.{
            available, featured, reservable,
        } == true
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    let spread = r#"
        fields flags = {
            available, featured, reservable,
        }

        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.{...flags} == true
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    let inline_out = run_surface(&doc, inline);
    let spread_out = run_surface(&doc, spread);
    assert_eq!(inline_out, spread_out);
    assert!(!spread_out.is_empty());
}

#[test]
fn fields_macro_spread_with_override() {
    let doc = library_doc("override");
    let spread = r#"
        fields flags = {
            available, featured, reservable,
        }

        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.{...flags, reservable: false} == true
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    let inline = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.available == true
          and b.featured == true
          and b.reservable == false
        aggregate { total: sum(s.days.borrowed) } by s.branch
    "#;
    assert_eq!(run_surface(&doc, spread), run_surface(&doc, inline));
}

// ============================================================================
// select projection
// ============================================================================

#[test]
fn select_projection_emits_synthetic_objects() {
    let doc = library_doc("select");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        where b.shelf in ["A1", "A2"]
        select {
            shelf:  b.shelf,
            branch: s.branch,
            total:  s.days.borrowed,
            author: b.author,
        }
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 3, "got rows: {:?}", out);
    for row in &out {
        for field in ["shelf", "branch", "total", "author"] {
            assert!(
                row.contains(&format!("\"{}\":", field)),
                "row missing field {}: {}",
                field,
                row
            );
        }
    }
    let s1 = out.iter().find(|r| r.contains("\"total\": 100")).unwrap();
    assert!(s1.contains("\"shelf\": \"A1\""));
    assert!(s1.contains("\"author\": \"rowling\""));
}

#[test]
fn select_with_missing_join_emits_null() {
    let doc = library_doc("select_null");
    let q = r#"
        from .catalog.loans[] as s
        left join .catalog.books[] as b on b.id == s.book_id
        select {
            branch: s.branch,
            shelf:  b.shelf,
        }
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 8);
    for row in &out {
        assert!(row.contains("\"shelf\":"), "row: {}", row);
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
        r#"from .items[] as i where i.location ends_with "berlin" select { w: i.location }"#,
    );
    assert_eq!(ends.len(), 1);
    assert!(ends[0].contains("\"w\": \"loc_eu_berlin\""));

    let contains = run_surface(
        &doc,
        r#"from .items[] as i where i.location contains "_us_" select { w: i.location }"#,
    );
    assert_eq!(contains.len(), 2);

    let matches = run_surface(
        &doc,
        r#"from .items[] as i where i.client matches "acme_*" select { c: i.client }"#,
    );
    assert_eq!(matches.len(), 2);

    let eu = run_surface(
        &doc,
        r#"from .items[] as i where i.location matches "loc_eu_*" select { w: i.location }"#,
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
    let doc = library_doc("order_desc");
    let q = r#"
        from .catalog.loans[] as s
        select {
            book:  s.book_id,
            total: s.days.borrowed,
        }
        order by .total desc
        limit 3
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 3);
    let totals: Vec<_> = out
        .iter()
        .map(|r| extract_int(r, "total"))
        .collect();
    assert_eq!(totals, vec!["11000", "7000", "1000"]);
}

#[test]
fn order_by_default_direction_is_ascending() {
    let doc = library_doc("order_asc");
    let q = r#"
        from .catalog.loans[] as s
        select { total: s.days.borrowed }
        order by .total
        limit 3
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 3);
    let first_totals: Vec<_> = out
        .iter()
        .map(|r| extract_int(r, "total"))
        .collect();
    assert_eq!(first_totals, vec!["15", "50", "100"]);
}

#[test]
fn order_by_multiple_keys_with_tiebreak() {
    let doc = library_doc("order_multi");
    let q = r#"
        from .catalog.loans[] as s
        select {
            g: s.branch,
            w: s.days.borrowed,
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
        vec!["east", "east", "east", "east", "east", "east", "west", "west"]
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
    let doc = library_doc("agg_block");
    let q = r#"
        from .catalog.loans[] as s
        aggregate {
            total_days:   sum(s.days.borrowed),
            loan_count:   count(),
            avg_days:     avg(s.days.borrowed),
            peak:         max(s.days.borrowed),
        } by s.branch
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2, "{:?}", out);

    let east = out.iter().find(|r| r.starts_with("east → ")).unwrap();
    assert!(east.contains("\"total_days\": 12365"), "{}", east);
    assert!(east.contains("\"loan_count\": 6"), "{}", east);
    assert!(east.contains("\"peak\": 11000"), "{}", east);
    assert!(east.contains("\"avg_days\": 2060."), "{}", east);

    let west = out.iter().find(|r| r.starts_with("west → ")).unwrap();
    assert!(west.contains("\"total_days\": 7700"), "{}", west);
    assert!(west.contains("\"loan_count\": 2"), "{}", west);
    assert!(west.contains("\"peak\": 7000"), "{}", west);
    assert!(west.contains("\"avg_days\": 3850"), "{}", west);
}

#[test]
fn aggregate_block_conditional_reducer_via_where() {
    let doc = library_doc("agg_cond");
    let q = r#"
        from .catalog.loans[] as s
        join .catalog.books[] as b on b.id == s.book_id
        aggregate {
            mystery_days: sum(s.days.borrowed) where b.genre == "mystery",
            total_days:   sum(s.days.borrowed),
        } by s.branch
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 2);

    let east = out.iter().find(|r| r.starts_with("east → ")).unwrap();
    assert!(east.contains("\"mystery_days\": 11050"), "{}", east);
    assert!(east.contains("\"total_days\": 12365"), "{}", east);

    let west = out.iter().find(|r| r.starts_with("west → ")).unwrap();
    assert!(west.contains("\"mystery_days\": null"), "{}", west);
    assert!(west.contains("\"total_days\": 7700"), "{}", west);
}

#[test]
fn aggregate_block_then_order_then_limit() {
    let doc = library_doc("agg_top1");
    let q = r#"
        from .catalog.loans[] as s
        aggregate { total: sum(s.days.borrowed) } by s.branch
        order by .total desc
        limit 1
    "#;
    let out = run_surface(&doc, q);
    assert_eq!(out.len(), 1);
    let row = &out[0];
    assert!(row.starts_with("east → "), "{}", row);
    assert!(row.contains("\"total\": 12365"), "{}", row);
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

fn genre_doc(test_name: &str) -> Document {
    let path = write_tmp(
        &format!("engine_query_surface_genre_{}.json", test_name),
        r#"{"rows":[
            {"genre":"fiction","actual":120,"target":100},
            {"genre":"fiction","actual":80,"target":100},
            {"genre":"history","actual":50,"target":40},
            {"genre":"history","actual":60,"target":40},
            {"genre":"science","actual":200,"target":250}
        ]}"#,
    );
    Document::open(&path, None).unwrap()
}

// Regression: the use case the removed `partition`/`aggregate each` form
// served — per-bucket derived metrics — is still expressible with a
// grouped `aggregate { ... } by KEY`.
#[test]
fn grouped_aggregate_derives_per_bucket_metrics() {
    let doc = genre_doc("basic");
    let q = r#"
        from .rows[] as r
        let a = sum(r.actual),
            t = sum(r.target)
        aggregate {
            pct:    (a - t) / t * 100,
            delta:  a - t
        } by r.genre
    "#;
    let out = run_surface(&doc, q);
    // One row per genre bucket.
    // fiction: a=200, t=200 → pct=0, delta=0
    // history: a=110, t=80  → pct=37.5, delta=30
    // science: a=200, t=250 → pct=-20, delta=-50
    assert_eq!(out.len(), 3, "{:?}", out);
    let joined = out.join("\n");
    assert!(joined.contains("fiction"), "missing fiction: {:?}", out);
    assert!(joined.contains("history"), "missing history: {:?}", out);
    assert!(joined.contains("science"), "missing science: {:?}", out);
    assert!(joined.contains("37.5"), "missing history pct (37.5): {:?}", out);
    assert!(joined.contains("-50"), "missing science delta (-50): {:?}", out);
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
    let doc = library_doc("agg_no_by");
    let out = run_surface(
        &doc,
        "from .catalog.loans[] as s aggregate { total: sum(s.days.borrowed) }",
    );
    assert_eq!(out, vec!["total → 20065"]);
}

#[test]
fn aggregate_count_without_by_or_arg() {
    let doc = library_doc("count_no_by");
    let out = run_surface(
        &doc,
        "from .catalog.loans[] as s aggregate { n: count() }",
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
    let doc = library_doc("agg_block_no_by");
    let q = r#"
        from .catalog.loans[] as s
        aggregate {
            total: sum(s.days.borrowed),
            rows:  count(),
            peak:  max(s.days.borrowed),
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
    let doc = library_doc("agg_null");
    let q = r#"
        from .catalog.loans[] as s
        where s.branch == "unobtainable_branch"
        aggregate {
            total:        sum(s.days.borrowed),
            with_default: sum(s.days.borrowed) ?? 0,
            with_label:   sum(s.days.borrowed) ?? null,
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
            "group_a": {"items":[{"g":"day"},{"g":"day"},{"g":"week"}]},
            "group_b": {"items":[{"g":"day"}]},
            "group_c": {"unrelated":1}
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let out = run_surface(
        &doc,
        r#"from .data[].items[] as s where s.g == "day" aggregate { n: count() } by s.g"#,
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
            "us":   {"sites":[
                {"status":"ok",   "region_id":"us"},
                {"status":"warn", "region_id":"us"}
            ]},
            "eu":   {"sites":[
                {"status":"ok",   "region_id":"eu"},
                {"status":null,   "region_id":"eu"}
            ]},
            "apac": {"sites":[
                {"status":"ok"}
            ]}
        }}"#,
    );
    let doc = Document::open(&path, None).unwrap();
    let q = r#"
        from .regions[].sites[] as w
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
    let doc = genre_doc("alias_only");
    let sugar = r#"
        from .rows[] as r
        let a = sum(r.actual)
        aggregate {
            total: a,
            doubled: a * 2
        }
    "#;
    let direct = r#"
        from .rows[] as r
        aggregate {
            total: sum(r.actual),
            doubled: sum(r.actual) * 2
        }
    "#;
    assert_eq!(run_surface(&doc, sugar), run_surface(&doc, direct));
}

#[test]
fn alias_let_forward_chain() {
    let doc = genre_doc("alias_chain");
    let sugar = r#"
        from .rows[] as r
        let a = sum(r.actual),
            b = a + 1
        aggregate { result: b }
    "#;
    let direct = r#"
        from .rows[] as r
        aggregate { result: sum(r.actual) + 1 }
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
from .catalog.loans[] as s
join .catalog.books[] as b
  on b.id == s.book_id
where b.shelf == \"A1\"
and b.author == \"rowling\"
aggregate {
  total: sum(s.days.borrowed)
} by s.branch";
    let once = surface::format(canonical).expect("format ok");
    assert_eq!(once, canonical);
    let twice = surface::format(&once).expect("format ok");
    assert_eq!(twice, once);
}

#[test]
fn formatter_normalises_messy_input() {
    let messy = "from .catalog.loans[] as s join .catalog.books[] as b on b.id==s.book_id where b.shelf==\"A1\" and b.author==\"rowling\" aggregate{total:sum(s.days.borrowed)}by s.branch";
    let formatted = surface::format(messy).expect("format ok");
    let expected = "\
from .catalog.loans[] as s
join .catalog.books[] as b
  on b.id == s.book_id
where b.shelf == \"A1\"
and b.author == \"rowling\"
aggregate {
  total: sum(s.days.borrowed)
} by s.branch";
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
    let doc = library_doc("compile");
    let ast = query::compile(
        r#"from .catalog.loans[] as s where s.branch == "west" aggregate { total: sum(s.days.borrowed) } by s.branch"#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert!(out.error.is_none());
    let formatted = format_results(&doc, out.results);
    assert_eq!(formatted.len(), 1);
    assert!(formatted[0].starts_with("west → "), "{}", formatted[0]);
    assert!(formatted[0].contains("\"total\": 7700"), "{}", formatted[0]);
}

#[test]
fn scanned_rows_counts_source_emissions() {
    let doc = library_doc("scanned");
    let ast = query::compile("from .catalog.loans[] as s aggregate { n: count() }")
        .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.scanned_rows, 8);

    let ast = query::compile(
        r#"from .catalog.loans[] as s where s.branch == "west" aggregate { n: count() }"#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.scanned_rows, 8);
    assert_eq!(out.results.len(), 1);
}

#[test]
fn join_runs_one_lookup_per_row() {
    let doc = library_doc("lookup_calls");

    let ast = query::compile(
        "from .catalog.loans[] as s aggregate { n: count() } by s.branch",
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.lookup_calls, 0);

    let ast = query::compile(
        r#"
            from .catalog.loans[] as s
            join .catalog.books[] as b on b.id == s.book_id
            select { sh: b.shelf }
        "#,
    )
    .expect("compile ok");
    let out = evaluator::run(&doc, &ast, 0, 5000);
    assert_eq!(out.lookup_calls, 8);

    let ast = query::compile(
        r#"
            from .catalog.loans[] as s
            join .catalog.books[] as b on b.id == s.book_id
            where b.available == true and b.featured == true
            aggregate { n: count() } by s.branch
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
