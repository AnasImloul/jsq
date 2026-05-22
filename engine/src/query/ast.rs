//! Query AST. Surface-language source compiles to this shape via
//! `query::surface::lower`; the evaluator interprets it. Path nodes
//! (`Identity`/`Field`/`Index`/`Iterate`/`Descend`/`DescendField`)
//! match the structure of the user's source path; everything else is a
//! lowered clause or operator.

use super::grammar::kw;

#[derive(Clone, Debug)]
pub enum Ast {
    // Path
    Identity,
    Field(String),
    Index(i64),
    Iterate,
    Pipe(Box<Ast>, Box<Ast>),

    /// Recursive descent. Emits the input node and every descendant in
    /// document order — the engine analogue of jq's `..`. Spelled `.**`
    /// in the surface language.
    Descend,

    /// Optimised compilation of `Descend | Field(name)` — walks the
    /// subtree once and emits only descendants whose own object key
    /// equals `name`. Avoids materialising every intermediate node.
    DescendField(String),

    /// Optimised compilation of `Iterate | Field(name)` — for each
    /// child of the input container that's an object, extracts the
    /// member named `name` and emits its value. Saves the per-row
    /// `walk` dispatch + record lookup that the unfused
    /// `Pipe(Iterate, Field(name))` shape pays. The canonical Display
    /// is unchanged (`.[].name`), so the index registry sees the same
    /// key it would for the unfused form.
    IterateField(String),

    // Literals
    LitNumber(f64),
    LitString(String),
    LitBool(bool),
    LitNull,

    // Operators
    Compare(Box<Ast>, CompareOp, Box<Ast>),
    And(Box<Ast>, Box<Ast>),
    Or(Box<Ast>, Box<Ast>),
    Not,                              // postfix `| not`

    // Predicate filter — surface `where P` lowers to `Select(P)`.
    Select(Box<Ast>),

    /// Stateful aggregators. Each consumes a stream of values from the
    /// left side of a pipe and emits a single result. Non-numeric inputs
    /// are silently skipped by the numeric reducers (sum/min/max/avg);
    /// `count` accepts anything and just counts emissions.
    /// Outside a pipe-RHS position they act as identity.
    Sum,
    Min,
    Max,
    Avg,
    Count,

    /// Grouping postfix. `<expr> by <key>` partitions the LHS of the
    /// surrounding pipe by `key` (evaluated against each upstream value)
    /// and runs `expr` once per bucket — `expr` is detected to be a
    /// reducer (or a pipeline ending in one) so any aggregator composes
    /// naturally: `count by .role`, `(.age | avg) by .role`, etc.
    By(Box<Ast>, Box<Ast>),

    /// Foreign-reference resolver. `lookup(SOURCE; KEY_EXPR)` treats the
    /// current input as a key value, streams `SOURCE` against the
    /// document root, and emits each candidate whose `KEY_EXPR` evaluates
    /// equal (by scalar equality, same as `==`) to the input. Source is
    /// rooted at the document so foreign refs resolve against sibling
    /// arrays without needing a `$root` syntax. No-match drops the input
    /// from the pipeline; multiple matches are emitted as a stream.
    ///
    /// `source_canon` and `key_canon` are the canonical Display forms of
    /// `source` and `key`, computed once at AST-construction time. The
    /// runtime registry lookup (`IndexRegistry::get`) is keyed on those
    /// strings; caching them on the node avoids the per-row recursive
    /// `to_string()` walk over the source/key sub-ASTs.
    Lookup {
        source: Box<Ast>,
        key: Box<Ast>,
        source_canon: String,
        key_canon: String,
    },

    /// Composite group key for multi-key `by`. Evaluates each component
    /// against the current input and emits a single synthetic string
    /// formed by joining their group-key renderings with U+001F (unit
    /// separator). Only meaningful as the key arm of a surrounding `By`
    /// — outside that position it acts as a single-value emitter, which
    /// is harmless for autocomplete / preview pipelines.
    KeyTuple(Vec<Ast>),

    /// Object projection produced by surface `select { name: expr, ...}`.
    /// For each input, evaluates each field's expression and collects
    /// the *first* emission per field into a synthetic `Value::Object`,
    /// defaulting to `Value::Null` when an expression emits nothing.
    /// Field order in the output matches AST order.
    Project(Vec<(String, Ast)>),

    /// Stateful sort. Pipe-RHS only: buffers the LHS stream, sorts each
    /// row by the named key expressions in order, then forwards. Each
    /// key has a direction; ties fall through to the next key. Outside
    /// a pipe-RHS position it acts as identity.
    SortBy(Vec<(Box<Ast>, SortDir)>),

    /// Stateful row-cap. Pipe-RHS only: forwards the first `N` values
    /// from LHS and stops. Outside a pipe-RHS position it acts as
    /// identity.
    Limit(u64),

    /// Stateful dedupe. Pipe-RHS only: walks the LHS stream and emits
    /// each *fingerprint*-distinct value at most once. Engine nodes
    /// fingerprint by raw JSON bytes (zero-copy); synthetic scalars
    /// fingerprint by JSON-encoded form so `Value::Str("hello")`
    /// fingerprints the same as a node string `"hello"`. Outside a
    /// pipe-RHS position it acts as identity.
    Distinct,

    /// Postfix existence test. Walks the inner expression and emits
    /// `Bool(true)` iff it produces at least one value (regardless of
    /// whether that value is `null`). Distinct from `!= null` because a
    /// missing field emits nothing — `Exists` is the only way to
    /// distinguish "key absent" from "key present with null value".
    Exists(Box<Ast>),

    /// Pass-through that bumps the evaluator's "scanned rows" counter
    /// for every value its inner emits. Surface lowering wraps the
    /// source path with this so the popover can show how many rows the
    /// query actually walked before any filter / aggregate / sort. No
    /// effect on results — emissions flow through unchanged.
    Tap(Box<Ast>),

    /// Per-row binding. As a pipe stage between source and downstream
    /// per-row work: evaluates `value` against the upstream row, stores
    /// the first emission in the evaluator's binding map under `name`
    /// (or `Value::Null` if `value` emitted nothing), then forwards
    /// the row unchanged so subsequent stages see it. The binding
    /// stays live until the next `Let` for the same name overwrites
    /// it, or until the next `evaluator::run` resets the map. There's
    /// no scope pop — that's intentional: it lets downstream reducers
    /// (`By`, `AggregateBlock`, `SortBy`) read the binding while
    /// processing the same row inside their per-row callbacks.
    Let { name: String, value: Box<Ast> },

    /// Reads the current value of a binding by name. Emits nothing
    /// when the name is unbound (the surface only emits these for
    /// names declared via `with`, so this should only happen if a
    /// query references a binding before its `Let` ran — e.g. inside
    /// the source clause itself, which is on purpose unsupported).
    Var(String),

    /// Fused `BASE.{f1, f2, ...} == TARGET`. Emitted by the surface
    /// lowerer when every entry of a field-set compare shares the
    /// same target — the common case for cube rollup queries. Walks
    /// the resolved object's child chain *once*, marks each named
    /// field as it encounters them, and short-circuits to false on
    /// the first member whose value isn't equal to the target.
    /// Equivalent in semantics to the AND-of-Compares fan-out the
    /// non-fused lowering produces, just without the linear-per-field
    /// re-scan.
    FieldSetEquals {
        base: Box<Ast>,
        fields: Vec<String>,
        target: Box<Ast>,
    },

    /// Multi-aggregate. Pipe-RHS only: walks LHS, optionally partitions
    /// rows by a group key, and runs every reduction in `reductions`
    /// against the rows that landed in each bucket. Emits one
    /// `Value::Object` per bucket with the key column first (when
    /// present) and the reductions in their declared order.
    ///
    /// `group` is `None` for a no-`by` block — every row collapses
    /// into one synthetic bucket and a single object is emitted.
    ///
    /// Each reduction may carry an optional `where_pred` — when set,
    /// only rows for which the predicate is truthy participate in that
    /// reduction. `count` with `value=None` counts rows; `count` with
    /// `value=Some(_)` counts non-null emissions of the value expr.
    AggregateBlock {
        group: Option<AggGroup>,
        reductions: Vec<AggReduction>,
        /// Per-output trees. One entry per aggregate-block item. Each
        /// pair is `(top-level field name, tree)`. The tree is either a
        /// `Leaf` carrying an arithmetic expression over
        /// `Ast::ReducerSlot(i)` references, or an `Object` of further
        /// named sub-nodes — the recursive shape lets a single item
        /// emit a nested object without flattening to dotted keys.
        outputs: Vec<(String, AggOutputNode)>,
    },

    /// Binary arithmetic. Both sides reduce to a single numeric value
    /// (first emission wins for stream-valued operands). Non-numeric
    /// operands and divide-by-zero produce `Value::Null`.
    Binary {
        op: BinaryOp,
        lhs: Box<Ast>,
        rhs: Box<Ast>,
    },

    /// Unary negation. Same coercion rules as `Binary`.
    Neg(Box<Ast>),

    /// Reference to a finalised reducer slot inside an
    /// `AggregateBlock`'s `reductions` list. Only meaningful inside the
    /// `outputs` expression of an `AggregateBlock`; evaluated outside
    /// that scope it emits nothing.
    ReducerSlot(usize),

    /// Object literal — `{ k1: e1, k2: e2, ... }`. Each field's value
    /// is walked once; the first emission becomes the field's value
    /// in the emitted `Value::Object`. Used by surface object literals
    /// in `select` / `where` / arithmetic contexts; aggregate-block
    /// nested outputs use `AggOutputNode::Object` instead because the
    /// slot frame is only valid during the emit step.
    Object(Vec<(String, Ast)>),

    /// JSON type test — `VALUE is TYPE` or `VALUE is not TYPE`.
    /// Walks `value`; for each emission, emits a `Value::Bool` that's
    /// true iff the emission's kind matches `kind` (or doesn't, when
    /// `negated`). Multi-emission LHS produces multiple booleans, the
    /// same way `Compare` does — surrounding `Select` aggregates them
    /// via `is_truthy`, so the predicate is true when any emission
    /// satisfies the test.
    TypeTest {
        value: Box<Ast>,
        kind: JsonTypeKind,
        negated: bool,
    },

    /// Numeric `round(VALUE [, PRECISION])`. Both operands collapse to
    /// their first emission; non-numeric values produce null. When
    /// `precision` is `None` it defaults to 0 (integer rounding).
    Round {
        value: Box<Ast>,
        precision: Option<Box<Ast>>,
    },
}

/// One of the six JSON types, used as the RHS of `is` / `is not`.
/// Defined here (rather than in `surface::ast`) so the evaluator can
/// reference it without the engine AST depending on the surface
/// module. The surface lexicon (`string`, `number`, `bool`, `null`,
/// `array`, `object`) is encoded here too.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum JsonTypeKind {
    String,
    Number,
    Bool,
    Null,
    Array,
    Object,
}

impl JsonTypeKind {
    pub fn keyword(self) -> &'static str {
        match self {
            JsonTypeKind::String => "string",
            JsonTypeKind::Number => "number",
            JsonTypeKind::Bool   => "bool",
            JsonTypeKind::Null   => "null",
            JsonTypeKind::Array  => "array",
            JsonTypeKind::Object => "object",
        }
    }

    /// Parses a type-name identifier into a `JsonTypeKind`. Returns
    /// `None` for any other identifier so the parser can surface a
    /// friendlier error.
    pub fn from_keyword(s: &str) -> Option<Self> {
        match s {
            "string" => Some(JsonTypeKind::String),
            "number" => Some(JsonTypeKind::Number),
            "bool"   => Some(JsonTypeKind::Bool),
            "null"   => Some(JsonTypeKind::Null),
            "array"  => Some(JsonTypeKind::Array),
            "object" => Some(JsonTypeKind::Object),
            _ => None,
        }
    }
}

/// One node in an aggregate block's output tree. A `Leaf` evaluates its
/// arithmetic expression (with `??` fallback on null); an `Object`
/// recurses into named sub-nodes and emits a `Value::Object`.
#[derive(Clone, Debug)]
pub enum AggOutputNode {
    Leaf {
        expr: Box<Ast>,
        /// `?? DEFAULT` — fallback emitted when the leaf expression
        /// produces `Value::Null` (every reducer empty, divide-by-zero,
        /// or a non-numeric operand somewhere). Lowered from the
        /// surface item's `default` clause or an inner field's `??`.
        default: Option<Box<Ast>>,
    },
    Object(Vec<(String, AggOutputNode)>),
}

#[derive(Clone, Debug)]
pub struct AggGroup {
    pub name: String,
    pub key: Box<Ast>,
}

#[derive(Clone, Debug)]
pub struct AggReduction {
    /// Synthetic identifier — internal-only, used for canonical Display
    /// and debugging. The user-facing output name lives on the
    /// `AggOutput` entry that references this slot.
    pub name: String,
    pub op: ReducerOp,
    pub value: Option<Box<Ast>>,
    pub where_pred: Option<Box<Ast>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// The five reducer ops shared by `By` (single-aggregate shorthand) and
/// `AggregateBlock` (multi-aggregate). Lives in the AST module rather
/// than in the evaluator so AST nodes can reference it directly.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReducerOp {
    Sum,
    Min,
    Max,
    Avg,
    Count,
}

/// Arithmetic operators emitted by the surface lowerer. Same shape as
/// the surface `BinaryOp` — kept distinct so the engine AST has no
/// dependency on `surface::ast`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl std::fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `LHS contains RHS` — substring test on string values.
    Contains,
    /// `LHS starts_with RHS` — prefix test on string values.
    StartsWith,
    /// `LHS ends_with RHS` — suffix test on string values.
    EndsWith,
    /// `LHS matches RHS` — glob match: `*` is any run of characters,
    /// `?` is exactly one character. All other characters are literal.
    Matches,
}

impl std::fmt::Display for CompareOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CompareOp::Eq => "==",
            CompareOp::Ne => "!=",
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
            CompareOp::Contains => kw::CONTAINS,
            CompareOp::StartsWith => kw::STARTS_WITH,
            CompareOp::EndsWith => kw::ENDS_WITH,
            CompareOp::Matches => kw::MATCHES,
        })
    }
}

/// Canonical string form of an `Ast`. Used as the registry key for
/// `lookup` indexes — two queries that lower to structurally identical
/// ASTs canonicalize to the same string and share an index entry.
///
/// Pipe chains whose right-hand side is a chain-step (Field, Index,
/// Iterate) collapse back to dot/bracket notation, so `.foo[]`
/// round-trips to `.foo[]` rather than `.foo | []`.
impl std::fmt::Display for Ast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ast::Identity => f.write_str("."),
            Ast::Field(name) => write!(f, ".{}", json_quote(name)),
            Ast::Index(i) => write!(f, ".[{}]", i),
            Ast::Iterate => f.write_str(".[*]"),
            Ast::Pipe(l, r) => {
                if is_chain_step(r) {
                    write!(f, "{}", l)?;
                    write_chain_step(r, f)
                } else {
                    write!(f, "{} | {}", l, r)
                }
            }
            Ast::Descend => f.write_str(".**"),
            Ast::DescendField(name) => write!(f, ".**.{}", json_quote(name)),
            // Render as the unfused chain so index-registry keys stay
            // stable: `.[].name` instead of an internal-only spelling.
            Ast::IterateField(name) => write!(f, ".[].{}", json_quote(name)),
            Ast::LitNumber(n) => {
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e16 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{}", n)
                }
            }
            Ast::LitString(s) => write!(f, "\"{}\"", escape_string(s)),
            Ast::LitBool(b) => f.write_str(if *b { kw::TRUE } else { kw::FALSE }),
            Ast::LitNull => f.write_str(kw::NULL),
            Ast::Compare(l, op, r) => write!(f, "{} {} {}", l, op, r),
            Ast::And(l, r) => write!(f, "{} {} {}", l, kw::AND, r),
            Ast::Or(l, r) => write!(f, "{} {} {}", l, kw::OR, r),
            Ast::Not => f.write_str(kw::NOT),
            Ast::Select(inner) => write!(f, "{}({})", kw::SELECT, inner),
            Ast::Sum => f.write_str(kw::SUM),
            Ast::Min => f.write_str(kw::MIN),
            Ast::Max => f.write_str(kw::MAX),
            Ast::Avg => f.write_str(kw::AVG),
            Ast::Count => f.write_str(kw::COUNT),
            Ast::By(expr, key) => write!(f, "{} {} {}", expr, kw::BY, key),
            Ast::Lookup { source_canon, key_canon, .. } => {
                // Engine-internal canonical form. The surface language
                // dropped the `lookup(...)` builtin in favour of `join`,
                // but the engine still implements joins via `Ast::Lookup`
                // — this is the debug spelling.
                write!(f, "lookup({}; {})", source_canon, key_canon)
            }
            Ast::KeyTuple(parts) => {
                f.write_str("(")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                f.write_str(")")
            }
            Ast::Project(fields) => {
                f.write_str("{")?;
                for (i, (name, expr)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", json_quote(name), expr)?;
                }
                f.write_str("}")
            }
            Ast::SortBy(keys) => {
                f.write_str("sort_by(")?;
                for (i, (k, dir)) in keys.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", k)?;
                    if matches!(dir, SortDir::Desc) {
                        f.write_str(" desc")?;
                    }
                }
                f.write_str(")")
            }
            Ast::Exists(inner) => write!(f, "({} exists)", inner),
            // Tap is a runtime pass-through; mirror the inner so the
            // canonical Display stays stable (the index-registry hash
            // doesn't see scanning instrumentation).
            Ast::Tap(inner) => write!(f, "{}", inner),
            Ast::Let { name, value } => write!(f, "let({} = {})", json_quote(name), value),
            Ast::Var(name) => write!(f, "${}", json_quote(name)),
            Ast::FieldSetEquals { base, fields, target } => {
                write!(f, "{}.{{", base)?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 { f.write_str(", ")?; }
                    write!(f, "{}", json_quote(field))?;
                }
                write!(f, "}} == {}", target)
            }
            Ast::Limit(n) => write!(f, "limit({})", n),
            Ast::Distinct => f.write_str(kw::DISTINCT),
            Ast::AggregateBlock { group, reductions, outputs } => {
                f.write_str("aggregate_block(")?;
                let mut first = true;
                if let Some(g) = group {
                    write!(f, "{}={}", json_quote(&g.name), g.key)?;
                    first = false;
                }
                for r in reductions {
                    if !first { f.write_str(", ")?; } else { first = false; }
                    write!(f, "{}=", json_quote(&r.name))?;
                    f.write_str(match r.op {
                        ReducerOp::Sum => kw::SUM,
                        ReducerOp::Min => kw::MIN,
                        ReducerOp::Max => kw::MAX,
                        ReducerOp::Avg => kw::AVG,
                        ReducerOp::Count => kw::COUNT,
                    })?;
                    if let Some(v) = &r.value {
                        write!(f, " {}", v)?;
                    }
                    if let Some(p) = &r.where_pred {
                        write!(f, " {} {}", kw::WHERE, p)?;
                    }
                }
                for (name, node) in outputs {
                    if !first { f.write_str(", ")?; } else { first = false; }
                    write!(f, "out({}=", json_quote(name))?;
                    write_agg_output_node(node, f)?;
                    f.write_str(")")?;
                }
                f.write_str(")")
            }
            Ast::Binary { op, lhs, rhs } => write!(f, "({} {} {})", lhs, op, rhs),
            Ast::Neg(inner) => write!(f, "(-{})", inner),
            Ast::ReducerSlot(i) => write!(f, "$slot({})", i),
            Ast::Object(fields) => {
                f.write_str("{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 { f.write_str(", ")?; }
                    write!(f, "{}: {}", json_quote(k), v)?;
                }
                f.write_str("}")
            }
            Ast::TypeTest { value, kind, negated } => {
                if *negated {
                    write!(f, "({} is not {})", value, kind.keyword())
                } else {
                    write!(f, "({} is {})", value, kind.keyword())
                }
            }
            Ast::Round { value, precision } => {
                if let Some(p) = precision {
                    write!(f, "round({}, {})", value, p)
                } else {
                    write!(f, "round({})", value)
                }
            }
        }
    }
}

fn write_agg_output_node(
    node: &AggOutputNode,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    match node {
        AggOutputNode::Leaf { expr, default } => {
            write!(f, "{}", expr)?;
            if let Some(d) = default {
                write!(f, " ?? {}", d)?;
            }
            Ok(())
        }
        AggOutputNode::Object(fields) => {
            f.write_str("{")?;
            for (i, (k, child)) in fields.iter().enumerate() {
                if i > 0 { f.write_str(", ")?; }
                write!(f, "{}: ", json_quote(k))?;
                write_agg_output_node(child, f)?;
            }
            f.write_str("}")
        }
    }
}

fn is_chain_step(a: &Ast) -> bool {
    matches!(a, Ast::Field(_) | Ast::Index(_) | Ast::Iterate)
}

fn write_chain_step(a: &Ast, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match a {
        Ast::Field(name) => write!(f, ".{}", json_quote(name)),
        Ast::Index(i) => write!(f, "[{}]", i),
        Ast::Iterate => f.write_str("[*]"),
        _ => write!(f, "{}", a),
    }
}

fn json_quote(name: &str) -> String {
    // If `name` is a bare-identifier, emit it as-is; otherwise quote.
    let mut chars = name.chars();
    if let Some(c0) = chars.next() {
        if c0.is_alphabetic() || c0 == '_' {
            if chars.all(|c| c.is_alphanumeric() || c == '_') {
                return name.to_string();
            }
        }
    }
    format!("\"{}\"", escape_string(name))
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}
