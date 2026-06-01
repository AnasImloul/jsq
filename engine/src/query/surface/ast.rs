//! Surface-language AST. The user-facing query syntax parses to this
//! shape; the lowering pass (`super::lower`) compiles it to the engine's
//! existing `query::ast::Ast`.

use super::super::ast::CompareOp;

/// Top-level surface query. Clauses appear in pipeline order
/// (`fields* → from → join* → where → let* → distinct → aggregate
/// → select → order by → limit`).
#[derive(Clone, Debug)]
pub struct Query {
    /// `fields NAME = { f1, f2, ... }` declarations at the top of the
    /// query. Compile-time only: resolved during lowering, never make
    /// it into the engine AST.
    pub field_sets: Vec<FieldSetDef>,
    /// `from PATH as ALIAS` — mandatory source clause. The path is
    /// implicitly iterated (SQL `FROM table` semantics); the alias
    /// binds each emitted row.
    pub source: SourceClause,
    /// `join PATH as ALIAS on LEFT == RIGHT` — zero or more inner
    /// joins. Each join's source path is implicitly iterated; the
    /// predicate must be a single equality with exactly one side
    /// referencing the new alias and the other referencing prior
    /// aliases.
    pub joins: Vec<JoinClause>,
    /// `unnest EXPR as ALIAS` — zero or more array fan-out clauses,
    /// applied after the joins and before `where`. Each binds `ALIAS` to
    /// successive elements of the array `EXPR`, emitting one row per
    /// element (an empty / non-array `EXPR` drops the row).
    pub unnests: Vec<UnnestClause>,
    pub where_clause: Option<Predicate>,
    /// `distinct` clause — when present, the pipeline emits each row
    /// at most once after `where` filters.
    pub distinct: bool,
    /// Post-`where` `let NAME = EXPR (, NAME = EXPR)*`. RHS is an
    /// arbitrary expression (typically reducer arithmetic) substituted
    /// into each aggregate item at lowering time.
    pub alias_lets: Vec<AliasLet>,
    pub aggregate: Option<AggregateClause>,
    /// `having PRED` — post-aggregate filter over the reduced bucket-row
    /// stream. References aggregate output fields by identity path
    /// (`.n`, `.total`). Only meaningful with an `aggregate { ... }`
    /// clause present.
    pub having: Option<Predicate>,
    /// `select { name: expr, ... }`.
    pub project: Option<Vec<(String, Expr)>>,
    pub order_by: Vec<OrderKey>,
    pub limit: Option<u64>,
}

/// `from PATH as ALIAS`. The path is implicitly iterated at lower time.
#[derive(Clone, Debug)]
pub struct SourceClause {
    pub path: PathExpr,
    pub alias: String,
}

/// Inner (`join` / `inner join`) drops outer rows with no match; left
/// (`left join`) keeps them with the joined alias bound to null.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
}

/// `[inner|left] join PATH as ALIAS on LEFT == RIGHT`.
#[derive(Clone, Debug)]
pub struct JoinClause {
    pub path: PathExpr,
    pub alias: String,
    pub on: Predicate,
    pub kind: JoinKind,
}

/// `unnest EXPR as ALIAS`. `expr` evaluates (per upstream row) to the
/// array being flattened; `alias` binds each element for downstream
/// clauses.
#[derive(Clone, Debug)]
pub struct UnnestClause {
    pub expr: Expr,
    pub alias: String,
}

#[derive(Clone, Debug)]
pub struct OrderKey {
    pub expr: Expr,
    pub desc: bool,
}

/// `fields NAME = { f1, f2, ... }` — names a reusable field set spread
/// via `...NAME` inside a field-set comparison. Compile-time only.
#[derive(Clone, Debug)]
pub struct FieldSetDef {
    pub name: String,
    pub fields: Vec<String>,
}

/// `let NAME = EXPR` appearing after `where`/`distinct`. RHS is an
/// arbitrary expression (commonly arithmetic over reducer calls). The
/// lowerer substitutes every bare-name reference inside aggregate items
/// with the bound expression.
#[derive(Clone, Debug)]
pub struct AliasLet {
    pub name: String,
    pub expr: Expr,
}

/// Aggregate clause variants. Mutually exclusive per query.
#[derive(Clone, Debug)]
pub enum AggregateClause {
    /// `aggregate { name: REDUCER [where P] [?? D], ... } [by KEY]` —
    /// multiple named reductions over the upstream rows.
    Block(AggregateBlock),
    /// `collect by KEY` — collect-mode grouping (gather members, no
    /// reduction).
    Group(GroupClause),
}

#[derive(Clone, Debug)]
pub struct GroupClause {
    pub key: Expr,
}

#[derive(Clone, Debug)]
pub struct AggregateBlock {
    pub reductions: Vec<AggBlockItem>,
    /// `by KEY[, KEY ...]`. Empty when no `by` clause is present;
    /// length 2+ is lowered to an `Ast::KeyTuple` so the engine sees
    /// a single composite key (unless `rollup` is set — see below).
    pub group_by: Vec<Expr>,
    /// `by rollup(KEY, ...)`. When set, the keys in `group_by` form a
    /// rollup hierarchy: the engine emits one grouping per key prefix
    /// (full detail, each subtotal, and the grand total) instead of a
    /// single composite group. Rolled-up trailing keys render as `null`.
    pub rollup: bool,
}

#[derive(Clone, Debug)]
pub struct AggBlockItem {
    pub name: String,
    /// Output expression. May be a single `Expr::Reducer` or any
    /// arithmetic combination of reducer calls and scalar
    /// sub-expressions. The lowering pass walks this tree, hoists
    /// every `Expr::Reducer` into a dedicated engine reduction, and
    /// rewrites references with a slot placeholder.
    pub output: Expr,
    pub where_pred: Option<Expr>,
    /// `?? DEFAULT` — fallback expression evaluated when `output`
    /// returns null.
    pub default: Option<Expr>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AggOp {
    Sum,
    Count,
    Avg,
    Min,
    Max,
}

/// Boolean predicate appearing in a `where` clause. Comma-separated
/// predicates are stored as nested `And` so the lowering pass doesn't
/// need to special-case the comma.
pub type Predicate = Expr;

/// Path with a root and a list of segments. Roots are either the
/// document `.` (Identity) or a bare identifier referencing a `from` /
/// `join` alias (`s`, `s.warehouse_id`).
#[derive(Clone, Debug)]
pub struct PathExpr {
    pub root: PathRoot,
    pub segments: Vec<PathSeg>,
}

#[derive(Clone, Debug)]
pub enum PathRoot {
    /// Leading `.` — the document root in `from`/`join` source paths,
    /// or the current pipeline row elsewhere.
    Identity,
    /// Bare identifier — references a `from`/`join` alias by name.
    Name(String),
}

#[derive(Clone, Debug)]
pub enum PathSeg {
    Field(String),
    Index(i64),
    /// `[]` — emit each immediate child. Only iteration form
    /// in the surface language.
    Iterate,
    /// `.**` — recursive descent including the node itself.
    StarStar,
    /// `.{a, b, c}` — field-set. Only legal as the *last* segment of a
    /// path that's the LHS of a comparison (parser enforces).
    FieldSet(Vec<FieldSetItem>),
}

/// One item inside a field-set `.{ ... }`. Plain field names use the
/// surrounding compare's RHS; overrides bring their own RHS; spreads
/// reference a `let` binding by name.
#[derive(Clone, Debug)]
pub enum FieldSetItem {
    Field(String),
    Override(String, Expr),
    Spread(String),
}

/// Expressions inside `where`, `let`, `sum/avg/...`, projections, etc.
#[derive(Clone, Debug)]
pub enum Expr {
    Path(PathExpr),
    Lit(Lit),
    /// Query parameter `$name`. Replaced with a literal value at lowering
    /// time from the caller-supplied parameter map; an unbound parameter
    /// is a compile error.
    Param(String),
    /// Array literal `[e1, e2, ...]`. Lowers to `Ast::ArrayLit` as a
    /// general expression, and is also matched directly as the RHS of
    /// `in` / `not in` (membership expansion).
    Array(Vec<Expr>),

    /// Field-set comparison sugar: `BASE.{f1, f2, ...} OP RHS`.
    FieldSetCompare {
        base: PathExpr,
        items: Vec<FieldSetItem>,
        op: CompareOp,
        rhs: Box<Expr>,
    },

    Compare(Box<Expr>, CompareOp, Box<Expr>),
    In(Box<Expr>, Box<Expr>),
    NotIn(Box<Expr>, Box<Expr>),

    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),

    /// Postfix `EXPR exists`.
    Exists(Box<Expr>),

    /// Binary arithmetic.
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    /// Unary minus on a non-literal.
    Neg(Box<Expr>),

    /// Reducer call. Only legal as a sub-expression inside an
    /// `aggregate { ... }` block item's output or inside an alias
    /// `let NAME = EXPR` (which gets substituted into aggregate items).
    Reducer {
        op: AggOp,
        arg: Option<Box<Expr>>,
    },

    /// Object literal — `{ k1: e1, k2: e2, ... }`.
    Object(Vec<ObjectField>),

    /// `VALUE is TYPE` / `VALUE is not TYPE`.
    TypeTest {
        value: Box<Expr>,
        kind: JsonTypeKind,
        negated: bool,
    },

    /// Strict scalar function call — `name(arg, ...)`. Covers `round`,
    /// `length`, `lower`, `upper`, `abs`, `floor`, `ceil`, … (see
    /// `grammar::FUNCTIONS`). Arity is validated at parse time. The lazy
    /// `if(...)` builtin is a separate variant since it must not evaluate
    /// all of its arguments.
    Call(String, Vec<Expr>),

    /// Correlated subquery — a parenthesised full query used in an
    /// expression position: `( from … )`. The inner query runs against
    /// the document root, but may reference the enclosing query's
    /// `from`/`join`/`unnest` aliases (correlation). Composes with
    /// `exists` (correlated existence), `in` (membership over the
    /// subquery's emissions), comparison, and `select` (scalar subquery
    /// — first emission wins).
    Subquery(Box<Query>),

    /// `if(COND, THEN, ELSE)`. Evaluates `cond`'s first emission; if
    /// truthy (jq rule: only `null` / `false` are falsy) the result is
    /// `then_branch`, otherwise `else_branch`. Both branches are
    /// allowed to emit a stream — the chosen branch's emissions flow
    /// downstream unchanged. Reducer calls inside any of the three
    /// arms hoist normally when this expression appears in an
    /// aggregate-block output position.
    If {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
}

pub use super::super::ast::JsonTypeKind;

/// One field of an object literal.
#[derive(Clone, Debug)]
pub struct ObjectField {
    pub name: String,
    pub value: Expr,
    /// `?? EXPR` fallback when `value` evaluates to null. Only legal
    /// inside an aggregate-item output expression.
    pub default: Option<Expr>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
pub enum Lit {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
}
