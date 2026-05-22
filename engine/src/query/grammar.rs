//! Single source of truth for the surface query language vocabulary.
//!
//! Both the engine (lexer / parser) and the UI (Swift highlighter +
//! autocomplete, via FFI) read keyword strings, operator spellings,
//! and punctuation from this module. Adding a keyword here makes it
//! visible to the parser, the highlighter and the autocomplete in
//! lockstep — there is nowhere else to update.

/// Keyword strings as `&'static str` constants. The parser refers to
/// these instead of bare string literals so renaming a keyword is a
/// single-file change.
pub mod kw {
    /// Source-clause introducer: `from PATH as ALIAS`.
    pub const FROM: &str = "from";
    /// Alias binder used by `from … as ALIAS` and `join … as ALIAS`.
    pub const AS: &str = "as";
    /// Inner-join introducer: `join PATH as ALIAS on EXPR`.
    pub const JOIN: &str = "join";
    /// Join-predicate keyword: `join … on LEFT == RIGHT`.
    pub const ON: &str = "on";

    pub const LET: &str = "let";
    pub const WHERE: &str = "where";
    pub const AGGREGATE: &str = "aggregate";
    pub const SELECT: &str = "select";
    pub const ORDER: &str = "order";
    pub const LIMIT: &str = "limit";

    pub const BY: &str = "by";
    pub const GROUP: &str = "group";
    pub const ASC: &str = "asc";
    pub const DESC: &str = "desc";

    pub const AND: &str = "and";
    pub const OR: &str = "or";
    pub const NOT: &str = "not";

    pub const IN: &str = "in";
    pub const EXISTS: &str = "exists";

    /// Type-test operator. `VALUE is TYPE` / `VALUE is not TYPE`
    /// where TYPE is one of `string`, `number`, `bool`, `null`,
    /// `array`, `object`. Sits at the comparison-precedence rung.
    pub const IS: &str = "is";

    pub const MATCHES: &str = "matches";
    pub const STARTS_WITH: &str = "starts_with";
    pub const ENDS_WITH: &str = "ends_with";
    pub const CONTAINS: &str = "contains";

    // JSON-type names used as the RHS of `is` / `is not`. Not full
    // keywords outside the type-test position — `.string` still works
    // as a field access.
    pub const TYPE_STRING: &str = "string";
    pub const TYPE_NUMBER: &str = "number";
    pub const TYPE_BOOL: &str = "bool";
    pub const TYPE_ARRAY: &str = "array";
    pub const TYPE_OBJECT: &str = "object";
    // (TYPE_NULL reuses the existing NULL keyword.)

    pub const SUM: &str = "sum";
    pub const COUNT: &str = "count";
    pub const AVG: &str = "avg";
    pub const MIN: &str = "min";
    pub const MAX: &str = "max";

    pub const TRUE: &str = "true";
    pub const FALSE: &str = "false";
    pub const NULL: &str = "null";

    /// Numeric rounding builtin. `round(VALUE)` rounds to the nearest
    /// integer; `round(VALUE, PRECISION)` rounds to that many decimal
    /// places (negative precision rounds to tens/hundreds/…).
    pub const ROUND: &str = "round";

    /// Stream-deduping clause. `SOURCE [where P] distinct …` emits each
    /// row at most once (by raw-byte equality on engine nodes,
    /// JSON-encoded form on synthetics).
    pub const DISTINCT: &str = "distinct";

    /// Named-predicate block. `partition { name: PRED, ... }` declares
    /// the buckets consumed by `aggregate each partition as p => p.name`.
    pub const PARTITION: &str = "partition";
    /// Iteration introducer: `aggregate each partition as p => p.name`.
    pub const EACH: &str = "each";
}

/// Coarse category — drives highlighting style on the UI side and
/// groups keywords for the autocomplete engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeywordCategory {
    Clause,
    Boolean,
    Comparison,
    Quantifier,
    Sort,
    Reducer,
    Literal,
    Builtin,
}

/// Where in a query the keyword is grammatically valid. Used by the
/// autocomplete engine to filter the suggestion list at the cursor.
/// `Both` covers keywords like `aggregate` and `select` that introduce
/// a clause but can also start a fresh expression at the top of a
/// query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeywordRole {
    ValueStart,
    AfterExpression,
    Both,
}

pub struct Keyword {
    pub text: &'static str,
    pub category: KeywordCategory,
    pub role: KeywordRole,
}

impl Keyword {
    pub fn valid_at_value_start(&self) -> bool {
        matches!(self.role, KeywordRole::ValueStart | KeywordRole::Both)
    }

    pub fn valid_after_expression(&self) -> bool {
        matches!(self.role, KeywordRole::AfterExpression | KeywordRole::Both)
    }
}

pub const KEYWORDS: &[Keyword] = &[
    // Clause introducers
    Keyword { text: kw::FROM,      category: KeywordCategory::Clause, role: KeywordRole::ValueStart },
    Keyword { text: kw::JOIN,      category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::AS,        category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::ON,        category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::LET,       category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::WHERE,     category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::PARTITION, category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::AGGREGATE, category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::EACH,      category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::GROUP,     category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::SELECT,    category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::ORDER,     category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
    Keyword { text: kw::LIMIT,     category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },

    // Group-by / sort
    Keyword { text: kw::BY,   category: KeywordCategory::Sort, role: KeywordRole::AfterExpression },
    Keyword { text: kw::ASC,  category: KeywordCategory::Sort, role: KeywordRole::AfterExpression },
    Keyword { text: kw::DESC, category: KeywordCategory::Sort, role: KeywordRole::AfterExpression },

    // Boolean
    Keyword { text: kw::AND, category: KeywordCategory::Boolean, role: KeywordRole::AfterExpression },
    Keyword { text: kw::OR,  category: KeywordCategory::Boolean, role: KeywordRole::AfterExpression },
    Keyword { text: kw::NOT, category: KeywordCategory::Boolean, role: KeywordRole::ValueStart },

    // Quantifiers
    Keyword { text: kw::IN,     category: KeywordCategory::Quantifier, role: KeywordRole::AfterExpression },
    Keyword { text: kw::EXISTS, category: KeywordCategory::Quantifier, role: KeywordRole::AfterExpression },
    Keyword { text: kw::IS,     category: KeywordCategory::Quantifier, role: KeywordRole::AfterExpression },

    // Comparison-rung pattern operators (lex as Idents, surface as ops)
    Keyword { text: kw::MATCHES,     category: KeywordCategory::Comparison, role: KeywordRole::AfterExpression },
    Keyword { text: kw::STARTS_WITH, category: KeywordCategory::Comparison, role: KeywordRole::AfterExpression },
    Keyword { text: kw::ENDS_WITH,   category: KeywordCategory::Comparison, role: KeywordRole::AfterExpression },
    Keyword { text: kw::CONTAINS,    category: KeywordCategory::Comparison, role: KeywordRole::AfterExpression },

    // Reducers
    Keyword { text: kw::SUM,   category: KeywordCategory::Reducer, role: KeywordRole::ValueStart },
    Keyword { text: kw::COUNT, category: KeywordCategory::Reducer, role: KeywordRole::ValueStart },
    Keyword { text: kw::AVG,   category: KeywordCategory::Reducer, role: KeywordRole::ValueStart },
    Keyword { text: kw::MIN,   category: KeywordCategory::Reducer, role: KeywordRole::ValueStart },
    Keyword { text: kw::MAX,   category: KeywordCategory::Reducer, role: KeywordRole::ValueStart },

    // Literal-name keywords (parser maps to `Lit`)
    Keyword { text: kw::TRUE,  category: KeywordCategory::Literal, role: KeywordRole::ValueStart },
    Keyword { text: kw::FALSE, category: KeywordCategory::Literal, role: KeywordRole::ValueStart },
    Keyword { text: kw::NULL,  category: KeywordCategory::Literal, role: KeywordRole::ValueStart },

    // Built-in functions — parsed as special primaries
    Keyword { text: kw::ROUND,  category: KeywordCategory::Builtin, role: KeywordRole::ValueStart },

    // Pipeline transformer — appears as a clause after `where`/joins
    // and before `partition`/`aggregate`; never starts a query.
    Keyword { text: kw::DISTINCT, category: KeywordCategory::Clause, role: KeywordRole::AfterExpression },
];

/// O(n) lookup over `KEYWORDS`. Fine — there are < 30 entries and the
/// parser only checks a handful per call.
pub fn keyword(text: &str) -> Option<&'static Keyword> {
    KEYWORDS.iter().find(|k| k.text == text)
}

pub fn is_keyword(text: &str) -> bool {
    keyword(text).is_some()
}

pub fn keyword_category(text: &str) -> Option<KeywordCategory> {
    keyword(text).map(|k| k.category)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatorKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Bare `=`. Used by `with NAME = expr` and `let NAME = {…}`.
    Assign,
    Add,
    Sub,
    Mul,
    Div,
}

pub struct OperatorSpec {
    pub text: &'static str,
    pub kind: OperatorKind,
}

/// Order matters for greedy matching: longer operators come first so
/// `==` is preferred over `=` when both could match.
pub const OPERATORS: &[OperatorSpec] = &[
    OperatorSpec { text: "==", kind: OperatorKind::Eq },
    OperatorSpec { text: "!=", kind: OperatorKind::Ne },
    OperatorSpec { text: "<=", kind: OperatorKind::Le },
    OperatorSpec { text: ">=", kind: OperatorKind::Ge },
    OperatorSpec { text: "<",  kind: OperatorKind::Lt },
    OperatorSpec { text: ">",  kind: OperatorKind::Gt },
    OperatorSpec { text: "=",  kind: OperatorKind::Assign },
    // Arithmetic. `*` is already in `PUNCTUATION` as the splat token and
    // serves double duty as the multiplication operator; the parser
    // decides which by position.
    OperatorSpec { text: "+",  kind: OperatorKind::Add },
    OperatorSpec { text: "-",  kind: OperatorKind::Sub },
    OperatorSpec { text: "*",  kind: OperatorKind::Mul },
    OperatorSpec { text: "/",  kind: OperatorKind::Div },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PunctKind {
    Dot,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Semi,
    Pipe,
    Question,
    QuestionQuestion,
    LParen,
    RParen,
    Star,
    StarStar,
    /// `=>` — body introducer for `aggregate each partition as p => …`.
    FatArrow,
}

pub struct PunctSpec {
    pub text: &'static str,
    pub kind: PunctKind,
}

/// Order matters: `**`/`??`/`=>` come before `*`/`?` for greedy matching.
pub const PUNCTUATION: &[PunctSpec] = &[
    PunctSpec { text: "**", kind: PunctKind::StarStar },
    PunctSpec { text: "??", kind: PunctKind::QuestionQuestion },
    PunctSpec { text: "=>", kind: PunctKind::FatArrow },
    PunctSpec { text: ".",  kind: PunctKind::Dot },
    PunctSpec { text: "[",  kind: PunctKind::LBrack },
    PunctSpec { text: "]",  kind: PunctKind::RBrack },
    PunctSpec { text: "{",  kind: PunctKind::LBrace },
    PunctSpec { text: "}",  kind: PunctKind::RBrace },
    PunctSpec { text: ":",  kind: PunctKind::Colon },
    PunctSpec { text: ",",  kind: PunctKind::Comma },
    PunctSpec { text: ";",  kind: PunctKind::Semi },
    PunctSpec { text: "|",  kind: PunctKind::Pipe },
    PunctSpec { text: "?",  kind: PunctKind::Question },
    PunctSpec { text: "(",  kind: PunctKind::LParen },
    PunctSpec { text: ")",  kind: PunctKind::RParen },
    PunctSpec { text: "*",  kind: PunctKind::Star },
];

/// Highlighter category emitted for each token by the FFI tokenizer.
/// Maps a `TokenKind` plus identifier classification to a UI bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenCategory {
    Keyword,
    Reducer,
    Literal,
    Identifier,
    String,
    Number,
    Comment,
    Operator,
    Splat,
    Punctuation,
    Error,
}

impl TokenCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenCategory::Keyword => "keyword",
            TokenCategory::Reducer => "reducer",
            TokenCategory::Literal => "literal",
            TokenCategory::Identifier => "identifier",
            TokenCategory::String => "string",
            TokenCategory::Number => "number",
            TokenCategory::Comment => "comment",
            TokenCategory::Operator => "operator",
            TokenCategory::Splat => "splat",
            TokenCategory::Punctuation => "punctuation",
            TokenCategory::Error => "error",
        }
    }
}

/// Maps an identifier to its UI category. Used by both the highlighter
/// (via the FFI tokenizer) and the autocomplete engine.
pub fn identifier_category(text: &str) -> TokenCategory {
    match keyword_category(text) {
        Some(KeywordCategory::Reducer) => TokenCategory::Reducer,
        Some(KeywordCategory::Literal) => TokenCategory::Literal,
        Some(_) => TokenCategory::Keyword,
        None => TokenCategory::Identifier,
    }
}

/// JSON dump of the manifest, eaten by the Swift side.
///
/// Stable shape:
/// ```json
/// {
///   "keywords": [{"text": "where", "category": "clause", "role": "afterExpression"}, ...],
///   "operators": [{"text": "==", "kind": "eq"}, ...],
///   "punctuation": [{"text": ".", "kind": "dot"}, ...]
/// }
/// ```
pub fn manifest_json() -> String {
    let mut out = String::with_capacity(2048);
    out.push('{');

    out.push_str("\"keywords\":[");
    for (i, k) in KEYWORDS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"text\":\"");
        push_json_escaped(&mut out, k.text);
        out.push_str("\",\"category\":\"");
        out.push_str(keyword_category_str(k.category));
        out.push_str("\",\"role\":\"");
        out.push_str(keyword_role_str(k.role));
        out.push_str("\"}");
    }
    out.push(']');

    out.push_str(",\"operators\":[");
    for (i, op) in OPERATORS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"text\":\"");
        push_json_escaped(&mut out, op.text);
        out.push_str("\",\"kind\":\"");
        out.push_str(operator_kind_str(op.kind));
        out.push_str("\"}");
    }
    out.push(']');

    out.push_str(",\"punctuation\":[");
    for (i, p) in PUNCTUATION.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"text\":\"");
        push_json_escaped(&mut out, p.text);
        out.push_str("\",\"kind\":\"");
        out.push_str(punct_kind_str(p.kind));
        out.push_str("\"}");
    }
    out.push(']');

    out.push('}');
    out
}

fn keyword_category_str(c: KeywordCategory) -> &'static str {
    match c {
        KeywordCategory::Clause => "clause",
        KeywordCategory::Boolean => "boolean",
        KeywordCategory::Comparison => "comparison",
        KeywordCategory::Quantifier => "quantifier",
        KeywordCategory::Sort => "sort",
        KeywordCategory::Reducer => "reducer",
        KeywordCategory::Literal => "literal",
        KeywordCategory::Builtin => "builtin",
    }
}

fn keyword_role_str(r: KeywordRole) -> &'static str {
    match r {
        KeywordRole::ValueStart => "valueStart",
        KeywordRole::AfterExpression => "afterExpression",
        KeywordRole::Both => "both",
    }
}

fn operator_kind_str(k: OperatorKind) -> &'static str {
    match k {
        OperatorKind::Eq => "eq",
        OperatorKind::Ne => "ne",
        OperatorKind::Lt => "lt",
        OperatorKind::Le => "le",
        OperatorKind::Gt => "gt",
        OperatorKind::Ge => "ge",
        OperatorKind::Assign => "assign",
        OperatorKind::Add => "add",
        OperatorKind::Sub => "sub",
        OperatorKind::Mul => "mul",
        OperatorKind::Div => "div",
    }
}

fn punct_kind_str(k: PunctKind) -> &'static str {
    match k {
        PunctKind::Dot => "dot",
        PunctKind::LBrack => "lbrack",
        PunctKind::RBrack => "rbrack",
        PunctKind::LBrace => "lbrace",
        PunctKind::RBrace => "rbrace",
        PunctKind::Colon => "colon",
        PunctKind::Comma => "comma",
        PunctKind::Semi => "semi",
        PunctKind::Pipe => "pipe",
        PunctKind::Question => "question",
        PunctKind::QuestionQuestion => "questionQuestion",
        PunctKind::LParen => "lparen",
        PunctKind::RParen => "rparen",
        PunctKind::Star => "star",
        PunctKind::StarStar => "starStar",
        PunctKind::FatArrow => "fatArrow",
    }
}

fn push_json_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
}
