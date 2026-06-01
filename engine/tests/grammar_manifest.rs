//! Invariants over the grammar manifest. The Rust parser and the Swift
//! UI both read from `query::grammar`; these tests are the only
//! mechanism that prevents the manifest's serialised JSON shape from
//! drifting away from what the parser actually accepts.

use engine::query::grammar::{
    self, KeywordCategory, KeywordRole, OperatorKind, PunctKind, KEYWORDS, OPERATORS, PUNCTUATION,
};

/// Every keyword the Rust parser hand-matches must be in the manifest.
/// If we add a new keyword to `surface/parser.rs` and forget to register
/// it here, the highlighter and autocomplete won't see it; this list is
/// the contract.
#[test]
fn all_parser_keywords_present_in_manifest() {
    use engine::query::grammar::kw;

    let expected = [
        kw::FROM, kw::FIELDS, kw::JOIN, kw::INNER, kw::LEFT, kw::AS, kw::ON, kw::UNNEST,
        kw::LET, kw::WHERE, kw::AGGREGATE, kw::COLLECT,
        kw::HAVING, kw::SELECT, kw::ORDER, kw::LIMIT,
        kw::BY, kw::ROLLUP, kw::ASC, kw::DESC,
        kw::AND, kw::OR, kw::NOT,
        kw::IN, kw::EXISTS, kw::IS,
        kw::MATCHES, kw::STARTS_WITH, kw::ENDS_WITH, kw::CONTAINS,
        kw::SUM, kw::COUNT, kw::AVG, kw::MIN, kw::MAX,
        kw::TRUE, kw::FALSE, kw::NULL,
        kw::ROUND,
        kw::IF,
        kw::LENGTH, kw::LOWER, kw::UPPER, kw::ABS, kw::FLOOR, kw::CEIL,
        kw::SQRT, kw::POW, kw::MOD, kw::TRIM, kw::SUBSTR, kw::REPLACE,
        kw::DISTINCT,
    ];
    for word in expected {
        assert!(
            grammar::is_keyword(word),
            "manifest is missing `{}` — add it to `KEYWORDS` in grammar.rs",
            word
        );
    }
}

/// And every manifest entry must actually be picked up by the parser
/// somewhere — otherwise the UI is suggesting a keyword the engine
/// will reject. Cheap proxy: the grammar `kw::*` constants are the
/// only thing the parser uses, and we round-trip through them in the
/// previous test, so if `KEYWORDS` contains an entry whose `text` isn't
/// one of those constants we fail.
#[test]
fn no_orphaned_manifest_entries() {
    use engine::query::grammar::kw;

    let live: &[&str] = &[
        kw::FROM, kw::FIELDS, kw::JOIN, kw::INNER, kw::LEFT, kw::AS, kw::ON, kw::UNNEST,
        kw::LET, kw::WHERE, kw::AGGREGATE, kw::COLLECT,
        kw::HAVING, kw::SELECT, kw::ORDER, kw::LIMIT,
        kw::BY, kw::ROLLUP, kw::ASC, kw::DESC,
        kw::AND, kw::OR, kw::NOT,
        kw::IN, kw::EXISTS, kw::IS,
        kw::MATCHES, kw::STARTS_WITH, kw::ENDS_WITH, kw::CONTAINS,
        kw::SUM, kw::COUNT, kw::AVG, kw::MIN, kw::MAX,
        kw::TRUE, kw::FALSE, kw::NULL,
        kw::ROUND,
        kw::IF,
        kw::LENGTH, kw::LOWER, kw::UPPER, kw::ABS, kw::FLOOR, kw::CEIL,
        kw::SQRT, kw::POW, kw::MOD, kw::TRIM, kw::SUBSTR, kw::REPLACE,
        kw::DISTINCT,
    ];
    for k in KEYWORDS {
        assert!(
            live.contains(&k.text),
            "manifest entry `{}` has no parser match — remove it or wire it up",
            k.text
        );
    }
}

#[test]
fn keyword_categories_are_self_consistent() {
    use engine::query::grammar::kw;
    // Reducers must be tagged Reducer.
    for word in [kw::SUM, kw::COUNT, kw::AVG, kw::MIN, kw::MAX] {
        assert_eq!(
            grammar::keyword(word).unwrap().category,
            KeywordCategory::Reducer,
            "`{}` should be tagged Reducer",
            word
        );
    }
    // Literal-name keywords must be tagged Literal.
    for word in [kw::TRUE, kw::FALSE, kw::NULL] {
        assert_eq!(
            grammar::keyword(word).unwrap().category,
            KeywordCategory::Literal,
        );
    }
    // Boolean operators.
    for word in [kw::AND, kw::OR, kw::NOT] {
        assert_eq!(
            grammar::keyword(word).unwrap().category,
            KeywordCategory::Boolean,
        );
    }
}

#[test]
fn manifest_json_is_well_formed() {
    let json = grammar::manifest_json();
    // Light sanity check — full schema verification lives on the Swift
    // side where the JSON is decoded into typed structs.
    assert!(json.starts_with('{'));
    assert!(json.ends_with('}'));
    assert!(json.contains("\"keywords\":["));
    assert!(json.contains("\"operators\":["));
    assert!(json.contains("\"punctuation\":["));
    // A few representative entries so a typo in the encoder fails fast.
    assert!(json.contains("\"text\":\"where\""));
    assert!(json.contains("\"text\":\"==\""));
    assert!(json.contains("\"text\":\".\""));
}

#[test]
fn operators_cover_all_compare_token_kinds() {
    let kinds = [
        OperatorKind::Eq,
        OperatorKind::Ne,
        OperatorKind::Lt,
        OperatorKind::Le,
        OperatorKind::Gt,
        OperatorKind::Ge,
        OperatorKind::Assign,
    ];
    for kind in kinds {
        assert!(
            OPERATORS.iter().any(|o| o.kind == kind),
            "manifest is missing operator kind {:?}",
            kind
        );
    }
}

#[test]
fn punctuation_lists_every_lexer_kind() {
    let kinds = [
        PunctKind::Dot, PunctKind::LBrack, PunctKind::RBrack, PunctKind::LBrace, PunctKind::RBrace,
        PunctKind::Colon, PunctKind::Comma, PunctKind::Semi, PunctKind::Pipe,
        PunctKind::Question, PunctKind::QuestionQuestion,
        PunctKind::LParen, PunctKind::RParen,
        PunctKind::Star, PunctKind::StarStar,
    ];
    for kind in kinds {
        assert!(
            PUNCTUATION.iter().any(|p| p.kind == kind),
            "manifest is missing punctuation kind {:?}",
            kind
        );
    }
}

#[test]
fn role_filters_match_existing_swift_lists() {
    // Acts as a regression test against the lists the Swift
    // AutocompleteContext hard-codes. If you intentionally narrow or
    // widen the set, update both lists below.
    let value_start_expected = [
        "from", "fields", "not", "null", "true", "false",
        "sum", "count", "avg", "min", "max",
        "round", "if",
        "length", "lower", "upper", "abs", "floor", "ceil",
        "sqrt", "pow", "mod", "trim", "substr", "replace",
    ];
    let after_expression_expected = [
        "and", "or", "by", "rollup",
        "where", "let", "order", "limit",
        "join", "inner", "left", "on", "as", "unnest",
        "aggregate", "collect",
        "having", "select",
        "in", "exists", "matches", "starts_with", "ends_with", "contains",
        "asc", "desc",
        "distinct",
    ];

    for word in value_start_expected {
        let kw = grammar::keyword(word)
            .unwrap_or_else(|| panic!("missing keyword `{}` from manifest", word));
        assert!(
            kw.valid_at_value_start(),
            "`{}` must be valid at value-start",
            word
        );
    }
    for word in after_expression_expected {
        let kw = grammar::keyword(word)
            .unwrap_or_else(|| panic!("missing keyword `{}` from manifest", word));
        assert!(
            kw.valid_after_expression(),
            "`{}` must be valid after-expression",
            word
        );
    }
}

// Suppress an unused-import warning that goes away once a `Both`-role
// keyword exists again. Keeping the import + this no-op test reserves
// the slot for that future shape without a config-flag dance.
#[test]
fn _unused_keyword_role_kept_for_future_use() {
    let _ = KeywordRole::Both;
}
