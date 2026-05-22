//! Surface AST → engine AST.
//!
//! Key lowering rules:
//!
//!   * `from PATH as ALIAS`: lower to `PATH | Iterate | Let { ALIAS = . }`.
//!     The implicit iteration is the SQL `FROM table` convention; the
//!     `Let` writes the current row into the binding map so downstream
//!     `ALIAS.field` paths resolve via `Ast::Var(ALIAS)`.
//!
//!   * `join PATH as ALIAS on LEFT == RIGHT`: identify which side of the
//!     equality references the new alias (the inner key) and which
//!     references prior aliases (the outer key). Lower to
//!     `Let { ALIAS = pipe(OUTER_KEY, Lookup(PATH | Iterate; INNER_KEY_alias_stripped)) }`.
//!
//!   * `where P` becomes `Select(P)`. Top-level And/Or chains are
//!     cost-reordered so cheap predicates short-circuit first.
//!
//!   * `partition { N: P, ... }` is stored on the side; consumed only by
//!     the `aggregate each partition` form, which expands into a
//!     standard `AggregateBlock` with one item per partition (predicate
//!     fused into the item's `where`, name substituted into any
//!     `ALIAS.name` reference inside the body).
//!
//!   * `let NAME = EXPR` (post-where) is alias substitution into the
//!     aggregate body at lowering time. Never reaches the engine.

use std::collections::HashMap;

use super::super::ast::{
    AggOutputNode, AggReduction, Ast, BinaryOp as AstBinaryOp, CompareOp, ReducerOp, SortDir,
};
use super::super::QueryError;
use super::ast::{
    AggBlockItem, AggOp, AggregateBlock, AggregateClause, AggregateShorthand, AliasLet, BinaryOp,
    EachPartitionClause, Expr, FieldSetItem, GroupClause, JoinClause, Lit, ObjectField,
    PartitionDef, PathExpr, PathRoot, PathSeg, Query, SourceClause,
};

/// Lowering context. `aliases` tracks runtime row bindings (`from`/`join`
/// aliases — read via `Ast::Var`); `lets` is the compile-time field-set
/// macro table.
struct Env {
    aliases: std::collections::HashSet<String>,
    lets: HashMap<String, Vec<String>>,
}

pub fn lower_query(q: Query) -> Result<Ast, QueryError> {
    let mut env = Env {
        aliases: std::collections::HashSet::new(),
        lets: HashMap::new(),
    };
    for lb in &q.lets {
        env.lets.insert(lb.name.clone(), lb.fields.clone());
    }

    // ----- source -----
    //
    // `from PATH as ALIAS` lowers to `PATH | Let { ALIAS = . }`. The
    // path emits whatever it emits — iteration is always explicit via
    // `[*]`, so there is exactly one spelling for iteration in the
    // surface language. The Tap wraps the source-emitting prefix so
    // "rows scanned" counts what the alias actually saw.
    let SourceClause {
        path: source_path,
        alias: source_alias,
    } = &q.source;
    let source_path_ast = lower_path(source_path, &env)?;
    let mut pipeline = Ast::Tap(Box::new(source_path_ast));
    pipeline = pipe(
        pipeline,
        Ast::Let {
            name: source_alias.clone(),
            value: Box::new(Ast::Identity),
        },
    );
    if env.aliases.contains(source_alias) {
        return Err(QueryError::new(
            0,
            format!("duplicate alias `{}`", source_alias),
        ));
    }
    env.aliases.insert(source_alias.clone());

    // ----- joins -----
    for JoinClause { path, alias, on } in &q.joins {
        if env.aliases.contains(alias) {
            return Err(QueryError::new(
                0,
                format!("duplicate alias `{}`", alias),
            ));
        }
        let lookup_value = lower_join_to_lookup(alias, path, on, &env)?;
        pipeline = pipe(
            pipeline,
            Ast::Let {
                name: alias.clone(),
                value: Box::new(lookup_value),
            },
        );
        env.aliases.insert(alias.clone());
    }

    // ----- where -----
    if let Some(pred) = &q.where_clause {
        let p = lower_expr(pred, &env)?;
        let p = reorder_predicate_chain(p);
        pipeline = pipe(pipeline, Ast::Select(Box::new(p)));
    }

    // ----- distinct -----
    if q.distinct {
        pipeline = pipe(pipeline, Ast::Distinct);
    }

    // ----- aggregate (with partition/alias_let preprocessing) -----
    //
    // `alias_lets` and `partition`s only attach to an aggregate block or
    // an `aggregate each partition` form — anywhere else they're an
    // unused declaration and should fail loudly.
    let needs_agg_block = matches!(
        q.aggregate,
        Some(AggregateClause::Block(_)) | Some(AggregateClause::EachPartition(_))
    );
    if !q.alias_lets.is_empty() && !needs_agg_block {
        return Err(QueryError::new(
            0,
            "`let NAME = EXPR` (post-where) only applies to an `aggregate { ... }` \
             or `aggregate each partition` clause"
                .into(),
        ));
    }
    if !q.partitions.is_empty()
        && !matches!(q.aggregate, Some(AggregateClause::EachPartition(_)))
    {
        return Err(QueryError::new(
            0,
            "`partition { ... }` only applies to an `aggregate each partition` clause"
                .into(),
        ));
    }
    if matches!(q.aggregate, Some(AggregateClause::EachPartition(_))) && q.partitions.is_empty() {
        return Err(QueryError::new(
            0,
            "`aggregate each partition` requires a preceding `partition { ... }` block"
                .into(),
        ));
    }

    if let Some(agg) = &q.aggregate {
        pipeline = match agg {
            AggregateClause::Shorthand(s) => lower_aggregate(pipeline, s, &env)?,
            AggregateClause::Block(b) => {
                let expanded = preprocess_alias_lets(b, &q.alias_lets, &env)?;
                lower_aggregate_block(pipeline, &expanded, &env)?
            }
            AggregateClause::Group(g) => lower_group(pipeline, g, &env)?,
            AggregateClause::EachPartition(ep) => {
                let expanded = expand_each_partition(ep, &q.partitions)?;
                let expanded = preprocess_alias_lets(&expanded, &q.alias_lets, &env)?;
                lower_aggregate_block(pipeline, &expanded, &env)?
            }
        };
    }

    // ----- select -----
    if let Some(fields) = &q.project {
        let mut entries = Vec::with_capacity(fields.len());
        for (name, expr) in fields {
            entries.push((name.clone(), lower_expr(expr, &env)?));
        }
        pipeline = pipe(pipeline, Ast::Project(entries));
    }

    // ----- order by -----
    if !q.order_by.is_empty() {
        let mut keys = Vec::with_capacity(q.order_by.len());
        for k in &q.order_by {
            let rewritten = rewrite_for_order_by(&k.expr, &env);
            let key_ast = lower_expr(&rewritten, &env)?;
            let dir = if k.desc { SortDir::Desc } else { SortDir::Asc };
            keys.push((Box::new(key_ast), dir));
        }
        pipeline = pipe(pipeline, Ast::SortBy(keys));
    }

    if let Some(n) = q.limit {
        pipeline = pipe(pipeline, Ast::Limit(n));
    }

    Ok(pipeline)
}

// ===== joins =====

/// Lowers `join PATH as ALIAS on LEFT_EXPR == RIGHT_EXPR` into the value
/// expression for a `Let { ALIAS = … }` stage.
///
/// One side of the equality must reference the new alias (the inner-row
/// key); the other must reference only prior aliases / the document
/// root (the outer-row key). The inner side is rewritten to strip the
/// alias prefix — the engine evaluates the index key against each
/// candidate row directly.
fn lower_join_to_lookup(
    alias: &str,
    path: &PathExpr,
    on: &Expr,
    env: &Env,
) -> Result<Ast, QueryError> {
    let (left, right) = match on {
        Expr::Compare(l, CompareOp::Eq, r) => (l.as_ref(), r.as_ref()),
        _ => {
            return Err(QueryError::new(
                0,
                "join `on` predicate must be a single equality `LEFT == RIGHT`".into(),
            ));
        }
    };

    let left_refs_new = references_alias(left, alias);
    let right_refs_new = references_alias(right, alias);

    let (outer_key, inner_key) = match (left_refs_new, right_refs_new) {
        (true, false) => (right, left),
        (false, true) => (left, right),
        (true, true) => {
            return Err(QueryError::new(
                0,
                format!(
                    "join `on` predicate must split `{}.field` against a prior-alias \
                     expression — both sides reference `{}`",
                    alias, alias
                ),
            ));
        }
        (false, false) => {
            return Err(QueryError::new(
                0,
                format!(
                    "join `on` predicate must reference the new alias `{}` on exactly one side",
                    alias
                ),
            ));
        }
    };

    // Outer key evaluates against the current pipeline row (with prior
    // aliases bound). Inner key strips the new alias's prefix so it
    // becomes Identity-rooted — that's what the engine's `Lookup`
    // evaluates against each candidate row.
    let outer_key_ast = lower_expr(outer_key, env)?;
    let inner_stripped = strip_alias_prefix(inner_key, alias);
    let inner_key_ast = lower_expr(&inner_stripped, env)?;

    let source_ast = lower_path(path, env)?;

    let source_canon = source_ast.to_string();
    let key_canon = inner_key_ast.to_string();

    Ok(pipe(
        outer_key_ast,
        Ast::Lookup {
            source: Box::new(source_ast),
            key: Box::new(inner_key_ast),
            source_canon,
            key_canon,
        },
    ))
}

/// True if `e` contains any `Path` whose root is `Name(alias)`.
fn references_alias(e: &Expr, alias: &str) -> bool {
    match e {
        Expr::Path(p) => matches!(&p.root, PathRoot::Name(n) if n == alias),
        Expr::Lit(_) => false,
        Expr::Array(xs) => xs.iter().any(|x| references_alias(x, alias)),
        Expr::FieldSetCompare { base, rhs, .. } => {
            matches!(&base.root, PathRoot::Name(n) if n == alias)
                || references_alias(rhs, alias)
        }
        Expr::Compare(l, _, r) | Expr::In(l, r) | Expr::NotIn(l, r) => {
            references_alias(l, alias) || references_alias(r, alias)
        }
        Expr::And(l, r) | Expr::Or(l, r) => {
            references_alias(l, alias) || references_alias(r, alias)
        }
        Expr::Not(i) | Expr::Exists(i) | Expr::Neg(i) => references_alias(i, alias),
        Expr::Binary { lhs, rhs, .. } => {
            references_alias(lhs, alias) || references_alias(rhs, alias)
        }
        Expr::Reducer { arg, .. } => arg.as_ref().is_some_and(|a| references_alias(a, alias)),
        Expr::Object(fields) => fields.iter().any(|f| {
            references_alias(&f.value, alias)
                || f.default.as_ref().is_some_and(|d| references_alias(d, alias))
        }),
        Expr::TypeTest { value, .. } => references_alias(value, alias),
        Expr::Round { value, precision } => {
            references_alias(value, alias)
                || precision.as_ref().is_some_and(|p| references_alias(p, alias))
        }
    }
}

/// Returns `e` with any `Path { root: Name(alias), … }` rewritten to
/// `Path { root: Identity, … }`. Used when lowering a join's inner key
/// so the alias prefix drops away.
fn strip_alias_prefix(e: &Expr, alias: &str) -> Expr {
    match e {
        Expr::Path(p) => Expr::Path(strip_alias_in_path(p, alias)),
        Expr::Lit(_) => e.clone(),
        Expr::Array(xs) => Expr::Array(xs.iter().map(|x| strip_alias_prefix(x, alias)).collect()),
        Expr::FieldSetCompare { base, items, op, rhs } => Expr::FieldSetCompare {
            base: strip_alias_in_path(base, alias),
            items: items.clone(),
            op: *op,
            rhs: Box::new(strip_alias_prefix(rhs, alias)),
        },
        Expr::Compare(l, op, r) => Expr::Compare(
            Box::new(strip_alias_prefix(l, alias)),
            *op,
            Box::new(strip_alias_prefix(r, alias)),
        ),
        Expr::In(l, r) => Expr::In(
            Box::new(strip_alias_prefix(l, alias)),
            Box::new(strip_alias_prefix(r, alias)),
        ),
        Expr::NotIn(l, r) => Expr::NotIn(
            Box::new(strip_alias_prefix(l, alias)),
            Box::new(strip_alias_prefix(r, alias)),
        ),
        Expr::And(l, r) => Expr::And(
            Box::new(strip_alias_prefix(l, alias)),
            Box::new(strip_alias_prefix(r, alias)),
        ),
        Expr::Or(l, r) => Expr::Or(
            Box::new(strip_alias_prefix(l, alias)),
            Box::new(strip_alias_prefix(r, alias)),
        ),
        Expr::Not(i) => Expr::Not(Box::new(strip_alias_prefix(i, alias))),
        Expr::Exists(i) => Expr::Exists(Box::new(strip_alias_prefix(i, alias))),
        Expr::Neg(i) => Expr::Neg(Box::new(strip_alias_prefix(i, alias))),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(strip_alias_prefix(lhs, alias)),
            rhs: Box::new(strip_alias_prefix(rhs, alias)),
        },
        Expr::Reducer { op, arg } => Expr::Reducer {
            op: *op,
            arg: arg.as_ref().map(|a| Box::new(strip_alias_prefix(a, alias))),
        },
        Expr::Object(fields) => Expr::Object(
            fields
                .iter()
                .map(|f| ObjectField {
                    name: f.name.clone(),
                    value: strip_alias_prefix(&f.value, alias),
                    default: f.default.as_ref().map(|d| strip_alias_prefix(d, alias)),
                })
                .collect(),
        ),
        Expr::TypeTest { value, kind, negated } => Expr::TypeTest {
            value: Box::new(strip_alias_prefix(value, alias)),
            kind: *kind,
            negated: *negated,
        },
        Expr::Round { value, precision } => Expr::Round {
            value: Box::new(strip_alias_prefix(value, alias)),
            precision: precision.as_ref().map(|p| Box::new(strip_alias_prefix(p, alias))),
        },
    }
}

fn strip_alias_in_path(p: &PathExpr, alias: &str) -> PathExpr {
    match &p.root {
        PathRoot::Name(n) if n == alias => PathExpr {
            root: PathRoot::Identity,
            segments: p.segments.clone(),
        },
        _ => p.clone(),
    }
}

// ===== each-partition expansion =====

/// Expands `aggregate each partition as p => p.name: { BODY }` into a
/// standard `AggregateBlock`: one `AggBlockItem` per declared partition,
/// where the item's name is the partition's name, its `where_pred` is
/// the partition's predicate, and its `output` is BODY with every
/// `ALIAS.name` reference substituted with the partition-name literal.
fn expand_each_partition(
    clause: &EachPartitionClause,
    partitions: &[PartitionDef],
) -> Result<AggregateBlock, QueryError> {
    let mut reductions = Vec::with_capacity(partitions.len());
    for p in partitions {
        let body = substitute_partition_name(&clause.body, &clause.partition_alias, &p.name);
        reductions.push(AggBlockItem {
            name: p.name.clone(),
            output: body,
            where_pred: Some(p.pred.clone()),
            default: None,
        });
    }
    Ok(AggregateBlock {
        reductions,
        group_by: None,
    })
}

/// Substitutes any `ALIAS.name` path (root=Name(alias), exactly one
/// segment Field("name")) inside `e` with a string-literal of the
/// partition name. Other paths rooted at `ALIAS` are illegal (the
/// parser already enforces `ALIAS.name` at the body header — but
/// re-checking inside lets the body itself reference `p.name`
/// elsewhere if the user wants to embed it).
fn substitute_partition_name(e: &Expr, alias: &str, partition_name: &str) -> Expr {
    match e {
        Expr::Path(p) => {
            if matches!(&p.root, PathRoot::Name(n) if n == alias)
                && p.segments.len() == 1
                && matches!(&p.segments[0], PathSeg::Field(f) if f == "name")
            {
                return Expr::Lit(Lit::Str(partition_name.to_string()));
            }
            Expr::Path(p.clone())
        }
        Expr::Lit(_) => e.clone(),
        Expr::Array(xs) => Expr::Array(
            xs.iter()
                .map(|x| substitute_partition_name(x, alias, partition_name))
                .collect(),
        ),
        Expr::FieldSetCompare { base, items, op, rhs } => Expr::FieldSetCompare {
            base: base.clone(),
            items: items.clone(),
            op: *op,
            rhs: Box::new(substitute_partition_name(rhs, alias, partition_name)),
        },
        Expr::Compare(l, op, r) => Expr::Compare(
            Box::new(substitute_partition_name(l, alias, partition_name)),
            *op,
            Box::new(substitute_partition_name(r, alias, partition_name)),
        ),
        Expr::In(l, r) => Expr::In(
            Box::new(substitute_partition_name(l, alias, partition_name)),
            Box::new(substitute_partition_name(r, alias, partition_name)),
        ),
        Expr::NotIn(l, r) => Expr::NotIn(
            Box::new(substitute_partition_name(l, alias, partition_name)),
            Box::new(substitute_partition_name(r, alias, partition_name)),
        ),
        Expr::And(l, r) => Expr::And(
            Box::new(substitute_partition_name(l, alias, partition_name)),
            Box::new(substitute_partition_name(r, alias, partition_name)),
        ),
        Expr::Or(l, r) => Expr::Or(
            Box::new(substitute_partition_name(l, alias, partition_name)),
            Box::new(substitute_partition_name(r, alias, partition_name)),
        ),
        Expr::Not(i) => Expr::Not(Box::new(substitute_partition_name(i, alias, partition_name))),
        Expr::Exists(i) => Expr::Exists(Box::new(substitute_partition_name(i, alias, partition_name))),
        Expr::Neg(i) => Expr::Neg(Box::new(substitute_partition_name(i, alias, partition_name))),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(substitute_partition_name(lhs, alias, partition_name)),
            rhs: Box::new(substitute_partition_name(rhs, alias, partition_name)),
        },
        Expr::Reducer { op, arg } => Expr::Reducer {
            op: *op,
            arg: arg
                .as_ref()
                .map(|a| Box::new(substitute_partition_name(a, alias, partition_name))),
        },
        Expr::Object(fields) => Expr::Object(
            fields
                .iter()
                .map(|f| ObjectField {
                    name: f.name.clone(),
                    value: substitute_partition_name(&f.value, alias, partition_name),
                    default: f
                        .default
                        .as_ref()
                        .map(|d| substitute_partition_name(d, alias, partition_name)),
                })
                .collect(),
        ),
        Expr::TypeTest { value, kind, negated } => Expr::TypeTest {
            value: Box::new(substitute_partition_name(value, alias, partition_name)),
            kind: *kind,
            negated: *negated,
        },
        Expr::Round { value, precision } => Expr::Round {
            value: Box::new(substitute_partition_name(value, alias, partition_name)),
            precision: precision
                .as_ref()
                .map(|p| Box::new(substitute_partition_name(p, alias, partition_name))),
        },
    }
}

// ===== group =====

fn lower_group(upstream: Ast, g: &GroupClause, env: &Env) -> Result<Ast, QueryError> {
    let key = lower_expr(&g.key, env)?;
    Ok(pipe(
        upstream,
        Ast::By(Box::new(Ast::Identity), Box::new(key)),
    ))
}

// ===== aggregate =====

fn lower_aggregate(
    upstream: Ast,
    agg: &AggregateShorthand,
    env: &Env,
) -> Result<Ast, QueryError> {
    let reducer_node = match agg.op {
        AggOp::Sum => Ast::Sum,
        AggOp::Count => Ast::Count,
        AggOp::Avg => Ast::Avg,
        AggOp::Min => Ast::Min,
        AggOp::Max => Ast::Max,
    };
    let reducer_expr = match (&agg.arg, agg.op) {
        (Some(e), _) => {
            let value = lower_expr(e, env)?;
            pipe(value, reducer_node.clone())
        }
        (None, AggOp::Count) => reducer_node.clone(),
        (None, _) => {
            return Err(QueryError::new(
                0,
                "this aggregate requires a value expression".into(),
            ));
        }
    };

    let key_ast = match agg.group_by.len() {
        0 => {
            return Ok(match &agg.arg {
                Some(e) => {
                    let value = lower_expr(e, env)?;
                    let with_value = pipe(upstream, value);
                    pipe(with_value, reducer_node)
                }
                None => pipe(upstream, reducer_node),
            });
        }
        1 => lower_expr(&agg.group_by[0], env)?,
        _ => {
            let mut parts = Vec::with_capacity(agg.group_by.len());
            for k in &agg.group_by {
                parts.push(lower_expr(k, env)?);
            }
            Ast::KeyTuple(parts)
        }
    };

    Ok(pipe(
        upstream,
        Ast::By(Box::new(reducer_expr), Box::new(key_ast)),
    ))
}

fn lower_aggregate_block(
    upstream: Ast,
    block: &AggregateBlock,
    env: &Env,
) -> Result<Ast, QueryError> {
    let group = if let Some(group_by) = &block.group_by {
        let name = group_key_label(group_by).unwrap_or_else(|| "key".to_string());
        let key = lower_expr(group_by, env)?;
        Some(super::super::ast::AggGroup {
            name,
            key: Box::new(key),
        })
    } else {
        None
    };

    let mut reductions: Vec<AggReduction> = Vec::new();
    let mut outputs: Vec<(String, AggOutputNode)> = Vec::with_capacity(block.reductions.len());
    for AggBlockItem {
        name,
        output,
        where_pred,
        default,
    } in &block.reductions
    {
        let item_where_ast = match where_pred {
            Some(p) => Some(Box::new(lower_expr(p, env)?)),
            None => None,
        };
        let node = lower_agg_output(
            output,
            default.as_ref(),
            &mut reductions,
            &item_where_ast,
            name,
            env,
        )?;
        outputs.push((name.clone(), node));
    }

    Ok(pipe(
        upstream,
        Ast::AggregateBlock {
            group,
            reductions,
            outputs,
        },
    ))
}

fn lower_agg_output(
    expr: &Expr,
    default: Option<&Expr>,
    reductions: &mut Vec<AggReduction>,
    where_pred_ast: &Option<Box<Ast>>,
    item_name: &str,
    env: &Env,
) -> Result<AggOutputNode, QueryError> {
    match expr {
        Expr::Object(fields) => {
            if default.is_some() {
                return Err(QueryError::new(
                    0,
                    "`??` default has no effect on an object-valued item — \
                     attach it to a leaf inside the object instead"
                        .into(),
                ));
            }
            let mut out: Vec<(String, AggOutputNode)> = Vec::with_capacity(fields.len());
            for f in fields {
                let child = lower_agg_output(
                    &f.value,
                    f.default.as_ref(),
                    reductions,
                    where_pred_ast,
                    &format!("{}_{}", item_name, f.name),
                    env,
                )?;
                out.push((f.name.clone(), child));
            }
            Ok(AggOutputNode::Object(out))
        }
        _ => {
            let expr_ast =
                lower_output_expr(expr, reductions, where_pred_ast, item_name, env)?;
            let default_ast = match default {
                Some(d) => Some(Box::new(lower_expr(d, env)?)),
                None => None,
            };
            Ok(AggOutputNode::Leaf {
                expr: Box::new(expr_ast),
                default: default_ast,
            })
        }
    }
}

/// Validates and substitutes the post-where `let NAME = EXPR` aliases
/// into the aggregate block's items. Each alias's RHS is itself
/// alias-substituted by earlier declarations so forward chaining works.
fn preprocess_alias_lets(
    block: &AggregateBlock,
    alias_lets: &[AliasLet],
    env: &Env,
) -> Result<AggregateBlock, QueryError> {
    let mut alias_map: HashMap<String, Expr> = HashMap::new();
    for al in alias_lets {
        if env.aliases.contains(&al.name) {
            return Err(QueryError::new(
                0,
                format!(
                    "alias `let {}` shadows a `from`/`join` alias of the same name",
                    al.name
                ),
            ));
        }
        if alias_map.contains_key(&al.name) {
            return Err(QueryError::new(
                0,
                format!("duplicate alias `let {}`", al.name),
            ));
        }
        let resolved = substitute_aliases(&al.expr, &alias_map);
        alias_map.insert(al.name.clone(), resolved);
    }

    let mut substituted: Vec<AggBlockItem> = Vec::with_capacity(block.reductions.len());
    for item in &block.reductions {
        substituted.push(AggBlockItem {
            name: item.name.clone(),
            output: substitute_aliases(&item.output, &alias_map),
            where_pred: item
                .where_pred
                .as_ref()
                .map(|e| substitute_aliases(e, &alias_map)),
            default: item
                .default
                .as_ref()
                .map(|e| substitute_aliases(e, &alias_map)),
        });
    }

    Ok(AggregateBlock {
        reductions: substituted,
        group_by: block.group_by.clone(),
    })
}

/// Substitutes bare-name path references (Name root, no segments)
/// against an alias map.
fn substitute_aliases(e: &Expr, aliases: &HashMap<String, Expr>) -> Expr {
    if aliases.is_empty() {
        return e.clone();
    }
    match e {
        Expr::Path(p) => {
            if p.segments.is_empty() {
                if let PathRoot::Name(n) = &p.root {
                    if let Some(replacement) = aliases.get(n) {
                        return replacement.clone();
                    }
                }
            }
            Expr::Path(p.clone())
        }
        Expr::Lit(_) => e.clone(),
        Expr::Array(items) => Expr::Array(
            items.iter().map(|x| substitute_aliases(x, aliases)).collect(),
        ),
        Expr::FieldSetCompare { base, items, op, rhs } => Expr::FieldSetCompare {
            base: base.clone(),
            items: items.clone(),
            op: *op,
            rhs: Box::new(substitute_aliases(rhs, aliases)),
        },
        Expr::Compare(l, op, r) => Expr::Compare(
            Box::new(substitute_aliases(l, aliases)),
            *op,
            Box::new(substitute_aliases(r, aliases)),
        ),
        Expr::In(l, r) => Expr::In(
            Box::new(substitute_aliases(l, aliases)),
            Box::new(substitute_aliases(r, aliases)),
        ),
        Expr::NotIn(l, r) => Expr::NotIn(
            Box::new(substitute_aliases(l, aliases)),
            Box::new(substitute_aliases(r, aliases)),
        ),
        Expr::And(l, r) => Expr::And(
            Box::new(substitute_aliases(l, aliases)),
            Box::new(substitute_aliases(r, aliases)),
        ),
        Expr::Or(l, r) => Expr::Or(
            Box::new(substitute_aliases(l, aliases)),
            Box::new(substitute_aliases(r, aliases)),
        ),
        Expr::Not(inner) => Expr::Not(Box::new(substitute_aliases(inner, aliases))),
        Expr::Exists(inner) => Expr::Exists(Box::new(substitute_aliases(inner, aliases))),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(substitute_aliases(lhs, aliases)),
            rhs: Box::new(substitute_aliases(rhs, aliases)),
        },
        Expr::Neg(inner) => Expr::Neg(Box::new(substitute_aliases(inner, aliases))),
        Expr::Reducer { op, arg } => Expr::Reducer {
            op: *op,
            arg: arg
                .as_ref()
                .map(|a| Box::new(substitute_aliases(a, aliases))),
        },
        Expr::Object(fields) => Expr::Object(
            fields
                .iter()
                .map(|f| ObjectField {
                    name: f.name.clone(),
                    value: substitute_aliases(&f.value, aliases),
                    default: f
                        .default
                        .as_ref()
                        .map(|d| substitute_aliases(d, aliases)),
                })
                .collect(),
        ),
        Expr::TypeTest { value, kind, negated } => Expr::TypeTest {
            value: Box::new(substitute_aliases(value, aliases)),
            kind: *kind,
            negated: *negated,
        },
        Expr::Round { value, precision } => Expr::Round {
            value: Box::new(substitute_aliases(value, aliases)),
            precision: precision
                .as_ref()
                .map(|p| Box::new(substitute_aliases(p, aliases))),
        },
    }
}

fn lower_output_expr(
    e: &Expr,
    reductions: &mut Vec<AggReduction>,
    where_pred_ast: &Option<Box<Ast>>,
    item_name: &str,
    env: &Env,
) -> Result<Ast, QueryError> {
    match e {
        Expr::Reducer { op, arg } => {
            let r_op = agg_op_to_reducer(*op);
            let value: Option<Box<Ast>> = match arg.as_ref() {
                Some(boxed) => Some(Box::new(lower_expr(boxed.as_ref(), env)?)),
                None => None,
            };
            let key = reducer_dedup_key(r_op, value.as_deref(), where_pred_ast.as_deref());
            if let Some(i) = reductions
                .iter()
                .position(|r| reducer_dedup_key(r.op, r.value.as_deref(), r.where_pred.as_deref()) == key)
            {
                return Ok(Ast::ReducerSlot(i));
            }
            let synthetic = format!("__{}_{}", item_name, reductions.len());
            reductions.push(AggReduction {
                name: synthetic,
                op: r_op,
                value,
                where_pred: where_pred_ast.clone(),
            });
            Ok(Ast::ReducerSlot(reductions.len() - 1))
        }
        Expr::Binary { op, lhs, rhs } => {
            let l = lower_output_expr(lhs, reductions, where_pred_ast, item_name, env)?;
            let r = lower_output_expr(rhs, reductions, where_pred_ast, item_name, env)?;
            Ok(Ast::Binary {
                op: map_binary_op(*op),
                lhs: Box::new(l),
                rhs: Box::new(r),
            })
        }
        Expr::Neg(inner) => {
            let i = lower_output_expr(inner, reductions, where_pred_ast, item_name, env)?;
            Ok(Ast::Neg(Box::new(i)))
        }
        Expr::Round { value, precision } => {
            let v = lower_output_expr(value, reductions, where_pred_ast, item_name, env)?;
            let p = match precision {
                Some(e) => Some(Box::new(lower_output_expr(
                    e, reductions, where_pred_ast, item_name, env,
                )?)),
                None => None,
            };
            Ok(Ast::Round { value: Box::new(v), precision: p })
        }
        _ => lower_expr(e, env),
    }
}

fn reducer_dedup_key(op: ReducerOp, value: Option<&Ast>, where_pred: Option<&Ast>) -> String {
    let value_canon = value.map(|a| a.to_string()).unwrap_or_default();
    let where_canon = where_pred.map(|a| a.to_string()).unwrap_or_default();
    format!("{:?}|{}|{}", op, value_canon, where_canon)
}

fn map_binary_op(op: BinaryOp) -> AstBinaryOp {
    match op {
        BinaryOp::Add => AstBinaryOp::Add,
        BinaryOp::Sub => AstBinaryOp::Sub,
        BinaryOp::Mul => AstBinaryOp::Mul,
        BinaryOp::Div => AstBinaryOp::Div,
    }
}

fn agg_op_to_reducer(op: AggOp) -> ReducerOp {
    match op {
        AggOp::Sum => ReducerOp::Sum,
        AggOp::Count => ReducerOp::Count,
        AggOp::Avg => ReducerOp::Avg,
        AggOp::Min => ReducerOp::Min,
        AggOp::Max => ReducerOp::Max,
    }
}

fn group_key_label(e: &Expr) -> Option<String> {
    let p = match e {
        Expr::Path(p) => p,
        _ => return None,
    };
    match p.segments.last() {
        Some(PathSeg::Field(name)) => Some(name.clone()),
        None => match &p.root {
            PathRoot::Name(n) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    }
}

// ===== expressions =====

fn lower_expr(e: &Expr, env: &Env) -> Result<Ast, QueryError> {
    match e {
        Expr::Path(p) => lower_path(p, env),
        Expr::Lit(l) => Ok(lower_lit(l)),
        Expr::Array(_) => Err(QueryError::new(
            0,
            "array literals are only supported as the right-hand side of `in` / `not in`".into(),
        )),
        Expr::FieldSetCompare { base, items, op, rhs } => {
            lower_field_set_compare(base, items, *op, rhs, env)
        }
        Expr::Compare(l, op, r) => {
            let la = lower_expr(l, env)?;
            let ra = lower_expr(r, env)?;
            Ok(Ast::Compare(Box::new(la), *op, Box::new(ra)))
        }
        Expr::In(l, r) => lower_in(l, r, false, env),
        Expr::NotIn(l, r) => lower_in(l, r, true, env),
        Expr::And(l, r) => {
            let la = lower_expr(l, env)?;
            let ra = lower_expr(r, env)?;
            Ok(Ast::And(Box::new(la), Box::new(ra)))
        }
        Expr::Or(l, r) => {
            let la = lower_expr(l, env)?;
            let ra = lower_expr(r, env)?;
            Ok(Ast::Or(Box::new(la), Box::new(ra)))
        }
        Expr::Not(inner) => {
            let inner_ast = lower_expr(inner, env)?;
            Ok(pipe(inner_ast, Ast::Not))
        }
        Expr::Exists(inner) => {
            let inner_ast = lower_expr(inner, env)?;
            Ok(Ast::Exists(Box::new(inner_ast)))
        }
        Expr::Binary { op, lhs, rhs } => {
            let l = lower_expr(lhs, env)?;
            let r = lower_expr(rhs, env)?;
            Ok(Ast::Binary {
                op: map_binary_op(*op),
                lhs: Box::new(l),
                rhs: Box::new(r),
            })
        }
        Expr::Neg(inner) => {
            let i = lower_expr(inner, env)?;
            Ok(Ast::Neg(Box::new(i)))
        }
        Expr::TypeTest { value, kind, negated } => {
            let inner = lower_expr(value, env)?;
            Ok(Ast::TypeTest {
                value: Box::new(inner),
                kind: *kind,
                negated: *negated,
            })
        }
        Expr::Round { value, precision } => {
            let v = lower_expr(value, env)?;
            let p = match precision {
                Some(e) => Some(Box::new(lower_expr(e, env)?)),
                None => None,
            };
            Ok(Ast::Round { value: Box::new(v), precision: p })
        }
        Expr::Reducer { .. } => Err(QueryError::new(
            0,
            "reducer calls (sum/count/avg/min/max) are only valid inside an \
             `aggregate { ... }` block item or an alias `let NAME = EXPR`"
                .into(),
        )),
        Expr::Object(fields) => {
            let mut out: Vec<(String, Ast)> = Vec::with_capacity(fields.len());
            for f in fields {
                if f.default.is_some() {
                    return Err(QueryError::new(
                        0,
                        "`??` default on an object-literal field is only \
                         supported inside an `aggregate { ... }` item"
                            .into(),
                    ));
                }
                out.push((f.name.clone(), lower_expr(&f.value, env)?));
            }
            Ok(Ast::Object(out))
        }
    }
}

fn lower_lit(l: &Lit) -> Ast {
    match l {
        Lit::Number(n) => Ast::LitNumber(*n),
        Lit::Str(s) => Ast::LitString(s.clone()),
        Lit::Bool(b) => Ast::LitBool(*b),
        Lit::Null => Ast::LitNull,
    }
}

fn lower_field_set_compare(
    base: &PathExpr,
    items: &[FieldSetItem],
    op: CompareOp,
    rhs: &Expr,
    env: &Env,
) -> Result<Ast, QueryError> {
    let base_ast = lower_path(base, env)?;
    let default_rhs = lower_expr(rhs, env)?;

    let mut entries: Vec<(String, Ast)> = Vec::new();
    let put = |name: String, value: Ast, entries: &mut Vec<(String, Ast)>| {
        if let Some(slot) = entries.iter_mut().find(|(n, _)| n == &name) {
            slot.1 = value;
        } else {
            entries.push((name, value));
        }
    };
    for item in items {
        match item {
            FieldSetItem::Field(name) => {
                put(name.clone(), default_rhs.clone(), &mut entries);
            }
            FieldSetItem::Spread(spread_name) => {
                let fields = env.lets.get(spread_name).ok_or_else(|| {
                    QueryError::new(
                        0,
                        format!("undefined `let` field set `{}`", spread_name),
                    )
                })?;
                for f in fields {
                    put(f.clone(), default_rhs.clone(), &mut entries);
                }
            }
            FieldSetItem::Override(name, value_expr) => {
                let v = lower_expr(value_expr, env)?;
                put(name.clone(), v, &mut entries);
            }
        }
    }

    if entries.is_empty() {
        return Err(QueryError::new(0, "empty field-set".into()));
    }

    if op == CompareOp::Eq && entries.len() > 1 {
        let first_target_canon = entries[0].1.to_string();
        let all_same = entries
            .iter()
            .skip(1)
            .all(|(_, ast)| ast.to_string() == first_target_canon);
        if all_same {
            let target_ast = entries[0].1.clone();
            let fields: Vec<String> = entries.into_iter().map(|(name, _)| name).collect();
            return Ok(Ast::FieldSetEquals {
                base: Box::new(base_ast),
                fields,
                target: Box::new(target_ast),
            });
        }
    }

    let mut acc: Option<Ast> = None;
    for (field, rhs_ast) in entries {
        let lhs = pipe(base_ast.clone(), Ast::Field(field));
        let cmp = Ast::Compare(Box::new(lhs), op, Box::new(rhs_ast));
        acc = Some(match acc {
            None => cmp,
            Some(prev) => Ast::And(Box::new(prev), Box::new(cmp)),
        });
    }
    Ok(acc.unwrap())
}

fn lower_in(lhs: &Expr, rhs: &Expr, negated: bool, env: &Env) -> Result<Ast, QueryError> {
    let items = match rhs {
        Expr::Array(xs) => xs,
        _ => {
            return Err(QueryError::new(
                0,
                "right-hand side of `in` must be an array literal".into(),
            ));
        }
    };
    if items.is_empty() {
        return Ok(Ast::LitBool(negated));
    }
    let lhs_ast = lower_expr(lhs, env)?;
    let (op, joiner): (CompareOp, fn(Ast, Ast) -> Ast) = if negated {
        (CompareOp::Ne, |a, b| Ast::And(Box::new(a), Box::new(b)))
    } else {
        (CompareOp::Eq, |a, b| Ast::Or(Box::new(a), Box::new(b)))
    };
    let mut acc: Option<Ast> = None;
    for item in items {
        let r = lower_expr(item, env)?;
        let cmp = Ast::Compare(Box::new(lhs_ast.clone()), op, Box::new(r));
        acc = Some(match acc {
            None => cmp,
            Some(prev) => joiner(prev, cmp),
        });
    }
    Ok(acc.unwrap())
}

// ===== paths =====

/// Path-only lowering for index-builder helpers. Bare paths can't
/// reference aliases — they're always document-rooted.
pub fn lower_path_only(p: &PathExpr) -> Result<Ast, QueryError> {
    let env = Env {
        aliases: std::collections::HashSet::new(),
        lets: HashMap::new(),
    };
    lower_path(p, &env)
}

fn lower_path(p: &PathExpr, env: &Env) -> Result<Ast, QueryError> {
    let root_ast = match &p.root {
        PathRoot::Identity => Ast::Identity,
        PathRoot::Name(name) => {
            if !env.aliases.contains(name) {
                return Err(QueryError::new(
                    0,
                    format!("undefined alias `{}`", name),
                ));
            }
            Ast::Var(name.clone())
        }
    };

    let mut acc = root_ast;
    for seg in &p.segments {
        acc = match seg {
            PathSeg::Field(name) => pipe(acc, Ast::Field(name.clone())),
            PathSeg::Index(i) => pipe(acc, Ast::Index(*i)),
            PathSeg::Iterate => pipe(acc, Ast::Iterate),
            PathSeg::StarStar => pipe(acc, Ast::Descend),
            PathSeg::FieldSet(_) => {
                return Err(QueryError::new(
                    0,
                    "field-set is only legal as the LHS of a comparison".into(),
                ));
            }
        };
    }
    Ok(acc)
}

fn pipe(l: Ast, r: Ast) -> Ast {
    if matches!(l, Ast::Identity) {
        return r;
    }
    if let (Ast::Descend, Ast::Field(name)) = (&l, &r) {
        return Ast::DescendField(name.clone());
    }
    if let (Ast::Iterate, Ast::Field(name)) = (&l, &r) {
        return Ast::IterateField(name.clone());
    }
    if let Ast::Pipe(a, b) = &l {
        if let (Ast::Descend, Ast::Field(name)) = (b.as_ref(), &r) {
            return Ast::Pipe(a.clone(), Box::new(Ast::DescendField(name.clone())));
        }
        if let (Ast::Iterate, Ast::Field(name)) = (b.as_ref(), &r) {
            return Ast::Pipe(a.clone(), Box::new(Ast::IterateField(name.clone())));
        }
    }
    Ast::Pipe(Box::new(l), Box::new(r))
}

// ===== order-by bare-name rewrite =====

/// Walks an `order by` expression and rewrites every `Path { root:
/// Name(n) }` whose `n` isn't a known alias into an Identity-rooted
/// Field path. Lets users write `order by weight` against a select
/// projection's `weight` field without spelling out `.weight`.
fn rewrite_for_order_by(e: &Expr, env: &Env) -> Expr {
    match e {
        Expr::Path(p) => Expr::Path(rewrite_path_for_order_by(p, env)),
        Expr::Lit(_) => e.clone(),
        Expr::Array(items) => Expr::Array(
            items.iter().map(|x| rewrite_for_order_by(x, env)).collect(),
        ),
        Expr::FieldSetCompare { base, items, op, rhs } => Expr::FieldSetCompare {
            base: rewrite_path_for_order_by(base, env),
            items: items.clone(),
            op: *op,
            rhs: Box::new(rewrite_for_order_by(rhs, env)),
        },
        Expr::Compare(l, op, r) => Expr::Compare(
            Box::new(rewrite_for_order_by(l, env)),
            *op,
            Box::new(rewrite_for_order_by(r, env)),
        ),
        Expr::In(l, r) => Expr::In(
            Box::new(rewrite_for_order_by(l, env)),
            Box::new(rewrite_for_order_by(r, env)),
        ),
        Expr::NotIn(l, r) => Expr::NotIn(
            Box::new(rewrite_for_order_by(l, env)),
            Box::new(rewrite_for_order_by(r, env)),
        ),
        Expr::And(l, r) => Expr::And(
            Box::new(rewrite_for_order_by(l, env)),
            Box::new(rewrite_for_order_by(r, env)),
        ),
        Expr::Or(l, r) => Expr::Or(
            Box::new(rewrite_for_order_by(l, env)),
            Box::new(rewrite_for_order_by(r, env)),
        ),
        Expr::Not(inner) => Expr::Not(Box::new(rewrite_for_order_by(inner, env))),
        Expr::Exists(inner) => Expr::Exists(Box::new(rewrite_for_order_by(inner, env))),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(rewrite_for_order_by(lhs, env)),
            rhs: Box::new(rewrite_for_order_by(rhs, env)),
        },
        Expr::Neg(inner) => Expr::Neg(Box::new(rewrite_for_order_by(inner, env))),
        Expr::Reducer { op, arg } => Expr::Reducer {
            op: *op,
            arg: arg.as_ref().map(|a| Box::new(rewrite_for_order_by(a, env))),
        },
        Expr::Object(fields) => Expr::Object(
            fields
                .iter()
                .map(|f| ObjectField {
                    name: f.name.clone(),
                    value: rewrite_for_order_by(&f.value, env),
                    default: f
                        .default
                        .as_ref()
                        .map(|d| rewrite_for_order_by(d, env)),
                })
                .collect(),
        ),
        Expr::TypeTest { value, kind, negated } => Expr::TypeTest {
            value: Box::new(rewrite_for_order_by(value, env)),
            kind: *kind,
            negated: *negated,
        },
        Expr::Round { value, precision } => Expr::Round {
            value: Box::new(rewrite_for_order_by(value, env)),
            precision: precision
                .as_ref()
                .map(|p| Box::new(rewrite_for_order_by(p, env))),
        },
    }
}

fn rewrite_path_for_order_by(p: &PathExpr, env: &Env) -> PathExpr {
    match &p.root {
        PathRoot::Name(name) if !env.aliases.contains(name) => {
            let mut segments = Vec::with_capacity(p.segments.len() + 1);
            segments.push(PathSeg::Field(name.clone()));
            segments.extend(p.segments.iter().cloned());
            PathExpr {
                root: PathRoot::Identity,
                segments,
            }
        }
        _ => p.clone(),
    }
}

// ===== predicate reorder =====

fn reorder_predicate_chain(ast: Ast) -> Ast {
    match ast {
        Ast::And(_, _) => {
            let mut conjuncts: Vec<Ast> = Vec::new();
            flatten_and(ast, &mut conjuncts);
            let mut indexed: Vec<(usize, u64, Ast)> = conjuncts
                .into_iter()
                .enumerate()
                .map(|(i, a)| (i, predicate_cost(&a), a))
                .collect();
            indexed.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            let mut iter = indexed.into_iter().map(|(_, _, a)| a);
            let first = iter.next().expect("flatten_and yields ≥1");
            iter.fold(first, |acc, x| Ast::And(Box::new(acc), Box::new(x)))
        }
        Ast::Or(_, _) => {
            let mut disjuncts: Vec<Ast> = Vec::new();
            flatten_or(ast, &mut disjuncts);
            let mut indexed: Vec<(usize, u64, Ast)> = disjuncts
                .into_iter()
                .enumerate()
                .map(|(i, a)| (i, predicate_cost(&a), a))
                .collect();
            indexed.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            let mut iter = indexed.into_iter().map(|(_, _, a)| a);
            let first = iter.next().expect("flatten_or yields ≥1");
            iter.fold(first, |acc, x| Ast::Or(Box::new(acc), Box::new(x)))
        }
        other => other,
    }
}

fn flatten_and(ast: Ast, out: &mut Vec<Ast>) {
    match ast {
        Ast::And(l, r) => {
            flatten_and(*l, out);
            flatten_and(*r, out);
        }
        other => out.push(other),
    }
}

fn flatten_or(ast: Ast, out: &mut Vec<Ast>) {
    match ast {
        Ast::Or(l, r) => {
            flatten_or(*l, out);
            flatten_or(*r, out);
        }
        other => out.push(other),
    }
}

fn predicate_cost(ast: &Ast) -> u64 {
    match ast {
        Ast::Identity
        | Ast::LitNumber(_)
        | Ast::LitString(_)
        | Ast::LitBool(_)
        | Ast::LitNull
        | Ast::Var(_) => 0,
        Ast::Field(_) | Ast::Index(_) => 1,
        Ast::Iterate | Ast::IterateField(_) => 8,
        Ast::Descend | Ast::DescendField(_) => 50,
        Ast::Pipe(l, r) => predicate_cost(l).saturating_add(predicate_cost(r)),
        Ast::Compare(l, _, r) => {
            1u64.saturating_add(predicate_cost(l)).saturating_add(predicate_cost(r))
        }
        Ast::And(l, r) | Ast::Or(l, r) => {
            predicate_cost(l).saturating_add(predicate_cost(r))
        }
        Ast::Not => 1,
        Ast::Select(inner) | Ast::Exists(inner) | Ast::Tap(inner) => predicate_cost(inner),
        Ast::Let { value, .. } => predicate_cost(value),
        Ast::Sum | Ast::Min | Ast::Max | Ast::Avg | Ast::Count | Ast::Limit(_) | Ast::Distinct => 1,
        Ast::Lookup { source, key, .. } => {
            30u64.saturating_add(predicate_cost(source)).saturating_add(predicate_cost(key))
        }
        Ast::FieldSetEquals { base, fields, target } => predicate_cost(base)
            .saturating_add(predicate_cost(target))
            .saturating_add(20)
            .saturating_add(10u64.saturating_mul(fields.len() as u64)),
        Ast::By(expr, key) => {
            500u64.saturating_add(predicate_cost(expr)).saturating_add(predicate_cost(key))
        }
        Ast::SortBy(_) => 500,
        Ast::AggregateBlock { .. } => 1000,
        Ast::Project(fields) => fields.iter().map(|(_, a)| predicate_cost(a)).sum::<u64>().saturating_add(5),
        Ast::KeyTuple(parts) => parts.iter().map(predicate_cost).sum(),
        Ast::Binary { lhs, rhs, .. } => 1u64
            .saturating_add(predicate_cost(lhs))
            .saturating_add(predicate_cost(rhs)),
        Ast::Neg(inner) => 1u64.saturating_add(predicate_cost(inner)),
        Ast::ReducerSlot(_) => 0,
        Ast::Object(fields) => fields
            .iter()
            .map(|(_, a)| predicate_cost(a))
            .sum::<u64>()
            .saturating_add(5),
        Ast::TypeTest { value, .. } => 1u64.saturating_add(predicate_cost(value)),
        Ast::Round { value, precision } => 2u64
            .saturating_add(predicate_cost(value))
            .saturating_add(precision.as_ref().map(|p| predicate_cost(p)).unwrap_or(0)),
    }
}
