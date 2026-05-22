//! Surface-language AST. The user-facing query syntax parses to this
//! shape; the lowering pass (`super::lower`) compiles it to the engine's
//! existing `query::ast::Ast`.

use super::super::ast::CompareOp;

/// Top-level surface query. Clauses appear in pipeline order
/// (`from → join* → where → let* → distinct → partition → aggregate
/// → select → order by → limit`).
#[derive(Clone, Debug)]
pub struct Query {
    /// `let NAME = { f1, f2, ... }` declarations at the top of the
    /// query. Compile-time only: resolved during lowering, never make
    /// it into the engine AST.
    pub lets: Vec<LetBinding>,
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
    pub where_clause: Option<Predicate>,
    /// `distinct` clause — when present, the pipeline emits each row
    /// at most once after `where` filters.
    pub distinct: bool,
    /// Post-`where` `let NAME = EXPR (, NAME = EXPR)*`. RHS is an
    /// arbitrary expression (typically reducer arithmetic) substituted
    /// into each aggregate item at lowering time.
    pub alias_lets: Vec<AliasLet>,
    /// `partition { name: PRED, ... }` — named row buckets consumed by
    /// `aggregate each partition as p => p.name`. Each partition's
    /// predicate is fused into the matching aggregate item's `where`.
    pub partitions: Vec<PartitionDef>,
    pub aggregate: Option<AggregateClause>,
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

/// `join PATH as ALIAS on LEFT == RIGHT`.
#[derive(Clone, Debug)]
pub struct JoinClause {
    pub path: PathExpr,
    pub alias: String,
    pub on: Predicate,
}

/// One entry inside `partition { ... }`. `name` keys the bucket; `pred`
/// is the row-level predicate that selects rows into it.
#[derive(Clone, Debug)]
pub struct PartitionDef {
    pub name: String,
    pub pred: Expr,
}

#[derive(Clone, Debug)]
pub struct OrderKey {
    pub expr: Expr,
    pub desc: bool,
}

/// `let NAME = { f1, f2, ... }` — names a reusable field set.
#[derive(Clone, Debug)]
pub struct LetBinding {
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
    /// `sum EXPR by K1, K2, ...` — single reducer, multi-key allowed.
    Shorthand(AggregateShorthand),
    /// `aggregate { name: REDUCER [where P] [?? D], ... } [by KEY]` —
    /// multiple named reductions over the upstream rows.
    Block(AggregateBlock),
    /// `group by KEY` — collect-mode grouping.
    Group(GroupClause),
    /// `aggregate each partition as ALIAS => ALIAS.name: { OBJECT }` —
    /// one item per declared partition, body shared.
    EachPartition(EachPartitionClause),
}

#[derive(Clone, Debug)]
pub struct GroupClause {
    pub key: Expr,
}

#[derive(Clone, Debug)]
pub struct AggregateShorthand {
    pub op: AggOp,
    pub arg: Option<Expr>,
    pub group_by: Vec<Expr>,
}

#[derive(Clone, Debug)]
pub struct AggregateBlock {
    pub reductions: Vec<AggBlockItem>,
    pub group_by: Option<Expr>,
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

/// `aggregate each partition as p => p.name: { BODY }`. The body is
/// applied once per declared partition; the partition's predicate
/// becomes the resulting item's `where` and the partition's name is
/// the item's output key.
#[derive(Clone, Debug)]
pub struct EachPartitionClause {
    /// The identifier in `as ALIAS` (e.g. `"p"`). Used only to validate
    /// the `ALIAS.name` accessor in the body — lowering substitutes any
    /// `ALIAS.name` path with a string literal of the current partition's
    /// name.
    pub partition_alias: String,
    /// The object expression on the RHS of `p.name:` — the per-partition
    /// output shape.
    pub body: Expr,
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
    /// `[*]` — emit each immediate child. Only iteration form
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
    /// Array literal — currently only used as the RHS of `in` / `not in`.
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

    /// `round(VALUE)` / `round(VALUE, PRECISION)`.
    Round {
        value: Box<Expr>,
        precision: Option<Box<Expr>>,
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
