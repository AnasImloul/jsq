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
//!   * `let NAME = EXPR` (post-where) is alias substitution into the
//!     aggregate body at lowering time. Never reaches the engine.

use std::collections::HashMap;

use super::super::ast::{
    AggOutputNode, AggReduction, Ast, BinaryOp as AstBinaryOp, CompareOp, ReducerOp, SortDir,
};
use super::super::QueryError;
use super::ast::{
    AggBlockItem, AggOp, AggregateBlock, AggregateClause, AliasLet, BinaryOp, Expr, FieldSetItem,
    GroupClause, JoinClause, JoinKind, Lit, ObjectField, PathExpr, PathRoot, PathSeg, Query,
    SourceClause, UnnestClause,
};

/// A value bound to a `$name` query parameter. Substituted in for the
/// matching `Expr::Param` during lowering.
#[derive(Clone, Debug)]
pub enum ParamValue {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
}

fn param_to_ast(v: &ParamValue) -> Ast {
    match v {
        ParamValue::Number(n) => Ast::LitNumber(*n),
        ParamValue::Str(s) => Ast::LitString(s.clone()),
        ParamValue::Bool(b) => Ast::LitBool(*b),
        ParamValue::Null => Ast::LitNull,
    }
}

/// Lowering context. `aliases` tracks runtime row bindings (`from`/`join`
/// aliases — read via `Ast::Var`); `lets` is the compile-time field-set
/// macro table; `params` holds caller-supplied `$name` bindings.
struct Env {
    aliases: std::collections::HashSet<String>,
    lets: HashMap<String, Vec<String>>,
    params: HashMap<String, ParamValue>,
}

pub fn lower_query(q: Query, params: &HashMap<String, ParamValue>) -> Result<Ast, QueryError> {
    lower_query_scoped(&q, params, &std::collections::HashSet::new())
}

/// Lowers a query whose `from`/`join`/`unnest` aliases start out with
/// `parent_aliases` already in scope. The top-level entry passes an empty
/// set; a correlated subquery (`Expr::Subquery`) passes the enclosing
/// query's aliases so inner paths like `o.x == c.id` resolve `c` and so a
/// reused alias name is rejected (which also keeps the runtime binding map
/// uncorrupted — the inner `Let` can never overwrite an outer binding).
fn lower_query_scoped(
    q: &Query,
    params: &HashMap<String, ParamValue>,
    parent_aliases: &std::collections::HashSet<String>,
) -> Result<Ast, QueryError> {
    let mut env = Env {
        aliases: parent_aliases.clone(),
        lets: HashMap::new(),
        params: params.clone(),
    };
    for fs in &q.field_sets {
        env.lets.insert(fs.name.clone(), fs.fields.clone());
    }

    // ----- source -----
    //
    // `from PATH as ALIAS` lowers to `PATH | Let { ALIAS = . }`. The
    // path emits whatever it emits — iteration is always explicit via
    // `[]`, so there is exactly one spelling for iteration in the
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
    for JoinClause { path, alias, on, kind } in &q.joins {
        if env.aliases.contains(alias) {
            return Err(QueryError::new(
                0,
                format!("duplicate alias `{}`", alias),
            ));
        }
        let join_node = lower_join_to_lookup(alias, path, on, *kind, &env)?;
        pipeline = pipe(pipeline, join_node);
        env.aliases.insert(alias.clone());
    }

    // ----- unnest -----
    //
    // `unnest EXPR as ALIAS` lowers to an `UnnestEach` fan-out stage: the
    // engine iterates the array `EXPR` produces and re-emits the row once
    // per element with `ALIAS` bound. The expression is lowered with the
    // aliases bound so far in scope, then the new alias joins the set so
    // subsequent clauses (and later unnests) can reference it.
    for UnnestClause { expr, alias } in &q.unnests {
        if env.aliases.contains(alias) {
            return Err(QueryError::new(0, format!("duplicate alias `{}`", alias)));
        }
        let source = lower_expr(expr, &env)?;
        pipeline = pipe(
            pipeline,
            Ast::UnnestEach {
                alias: alias.clone(),
                source: Box::new(source),
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

    // ----- aggregate (with alias_let preprocessing) -----
    //
    // `alias_lets` only attach to an `aggregate { ... }` block — anywhere
    // else they're an unused declaration and should fail loudly.
    let needs_agg_block = matches!(q.aggregate, Some(AggregateClause::Block(_)));
    if !q.alias_lets.is_empty() && !needs_agg_block {
        return Err(QueryError::new(
            0,
            "`let NAME = EXPR` (post-where) only applies to an `aggregate { ... }` clause"
                .into(),
        ));
    }

    if let Some(agg) = &q.aggregate {
        pipeline = match agg {
            AggregateClause::Block(b) => {
                let expanded = preprocess_alias_lets(b, &q.alias_lets, &env)?;
                lower_aggregate_block(pipeline, &expanded, &env)?
            }
            AggregateClause::Group(g) => lower_group(pipeline, g, &env)?,
        };
    }

    // ----- having -----
    //
    // Post-aggregate filter over the reduced bucket-row stream. Output
    // fields are addressed by identity path (`.n`), which the engine
    // resolves against `Value::BucketRow` by name.
    if let Some(pred) = &q.having {
        if q.aggregate.is_none() {
            return Err(QueryError::new(
                0,
                "`having` requires an `aggregate { ... }` clause".into(),
            ));
        }
        let p = lower_expr(pred, &env)?;
        pipeline = pipe(pipeline, Ast::Select(Box::new(p)));
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
            let key_ast = lower_expr(&k.expr, &env)?;
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
    kind: JoinKind,
    env: &Env,
) -> Result<Ast, QueryError> {
    // A join key is one equality or an `and`-chain of them (composite key).
    let mut equalities: Vec<(&Expr, &Expr)> = Vec::new();
    collect_join_equalities(on, &mut equalities)?;

    // Outer keys evaluate against the current pipeline row (with prior
    // aliases bound). Inner keys strip the new alias's prefix so they
    // become Identity-rooted — that's what the engine's `Lookup`
    // evaluates against each candidate row.
    let mut outer_keys: Vec<Ast> = Vec::with_capacity(equalities.len());
    let mut inner_keys: Vec<Ast> = Vec::with_capacity(equalities.len());
    for (left, right) in equalities {
        let (outer_key, inner_key) = split_join_equality(left, right, alias)?;
        outer_keys.push(lower_expr(outer_key, env)?);
        let inner_stripped = strip_alias_prefix(inner_key, alias);
        inner_keys.push(lower_expr(&inner_stripped, env)?);
    }

    // A single equality lowers to a flat scalar key; multiple wrap in a
    // KeyTuple so both sides render identically (U+001F-joined) and the
    // index's ScalarKey::Str match works without extra machinery.
    let (outer_key_ast, inner_key_ast) = if outer_keys.len() == 1 {
        (outer_keys.pop().unwrap(), inner_keys.pop().unwrap())
    } else {
        (Ast::KeyTuple(outer_keys), Ast::KeyTuple(inner_keys))
    };

    let source_ast = lower_path(path, env)?;

    let source_canon = source_ast.to_string();
    let key_canon = inner_key_ast.to_string();

    Ok(Ast::JoinEach {
        alias: alias.to_string(),
        outer_key: Box::new(outer_key_ast),
        lookup: Box::new(Ast::Lookup {
            source: Box::new(source_ast),
            key: Box::new(inner_key_ast),
            source_canon,
            key_canon,
        }),
        inner: matches!(kind, JoinKind::Inner),
    })
}

/// Flattens a join `on` predicate into its equality conjuncts: either a
/// single `LEFT == RIGHT` or an `and`-chain of them.
fn collect_join_equalities<'a>(
    on: &'a Expr,
    out: &mut Vec<(&'a Expr, &'a Expr)>,
) -> Result<(), QueryError> {
    match on {
        Expr::Compare(l, CompareOp::Eq, r) => {
            out.push((l.as_ref(), r.as_ref()));
            Ok(())
        }
        Expr::And(l, r) => {
            collect_join_equalities(l, out)?;
            collect_join_equalities(r, out)
        }
        _ => Err(QueryError::new(
            0,
            "join `on` predicate must be an equality `LEFT == RIGHT` or an \
             `and`-chain of equalities".into(),
        )),
    }
}

/// Splits one equality into `(outer, inner)` keys by which side
/// references the new alias.
fn split_join_equality<'a>(
    left: &'a Expr,
    right: &'a Expr,
    alias: &str,
) -> Result<(&'a Expr, &'a Expr), QueryError> {
    match (references_alias(left, alias), references_alias(right, alias)) {
        (true, false) => Ok((right, left)),
        (false, true) => Ok((left, right)),
        (true, true) => Err(QueryError::new(
            0,
            format!(
                "join `on` predicate must split `{}.field` against a prior-alias \
                 expression — both sides reference `{}`",
                alias, alias
            ),
        )),
        (false, false) => Err(QueryError::new(
            0,
            format!(
                "join `on` predicate must reference the new alias `{}` on exactly one side",
                alias
            ),
        )),
    }
}

/// True if `e` contains any `Path` whose root is `Name(alias)`.
fn references_alias(e: &Expr, alias: &str) -> bool {
    match e {
        Expr::Path(p) => matches!(&p.root, PathRoot::Name(n) if n == alias),
        Expr::Lit(_) | Expr::Param(_) => false,
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
        Expr::Call(_, args) => args.iter().any(|a| references_alias(a, alias)),
        Expr::If { cond, then_branch, else_branch } => {
            references_alias(cond, alias)
                || references_alias(then_branch, alias)
                || references_alias(else_branch, alias)
        }
        // A subquery is opaque to the join-key splitter: its own body
        // owns its alias scope, and join `on` predicates don't contain
        // subqueries, so it never references the join's new alias.
        Expr::Subquery(_) => false,
    }
}

/// Returns `e` with any `Path { root: Name(alias), … }` rewritten to
/// `Path { root: Identity, … }`. Used when lowering a join's inner key
/// so the alias prefix drops away.
fn strip_alias_prefix(e: &Expr, alias: &str) -> Expr {
    match e {
        Expr::Path(p) => Expr::Path(strip_alias_in_path(p, alias)),
        Expr::Lit(_) | Expr::Param(_) => e.clone(),
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
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter().map(|a| strip_alias_prefix(a, alias)).collect(),
        ),
        Expr::If { cond, then_branch, else_branch } => Expr::If {
            cond: Box::new(strip_alias_prefix(cond, alias)),
            then_branch: Box::new(strip_alias_prefix(then_branch, alias)),
            else_branch: Box::new(strip_alias_prefix(else_branch, alias)),
        },
        // Subqueries own their alias scope — the join inner-key rewrite
        // doesn't reach inside one. (Subqueries aren't part of a join's
        // `on` predicate in the surface, so this arm is conservative.)
        Expr::Subquery(_) => e.clone(),
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

// ===== group =====

fn lower_group(upstream: Ast, g: &GroupClause, env: &Env) -> Result<Ast, QueryError> {
    let key = lower_expr(&g.key, env)?;
    Ok(pipe(
        upstream,
        Ast::By(Box::new(Ast::Identity), Box::new(key)),
    ))
}

// ===== aggregate =====

fn lower_aggregate_block(
    upstream: Ast,
    block: &AggregateBlock,
    env: &Env,
) -> Result<Ast, QueryError> {
    use super::super::ast::{AggGroup, AggGroupKey};
    let group = if block.rollup {
        let mut keys = Vec::with_capacity(block.group_by.len());
        for (i, k) in block.group_by.iter().enumerate() {
            let name = group_key_label(k).unwrap_or_else(|| format!("key{}", i + 1));
            keys.push(AggGroupKey {
                name,
                key: Box::new(lower_expr(k, env)?),
            });
        }
        Some(AggGroup::Rollup(keys))
    } else {
        match block.group_by.as_slice() {
            [] => None,
            [single] => {
                let name = group_key_label(single).unwrap_or_else(|| "key".to_string());
                let key = lower_expr(single, env)?;
                Some(AggGroup::Single {
                    name,
                    key: Box::new(key),
                })
            }
            many => {
                let mut parts = Vec::with_capacity(many.len());
                for k in many {
                    parts.push(lower_expr(k, env)?);
                }
                let name = group_key_label(&many[0]).unwrap_or_else(|| "key".to_string());
                Some(AggGroup::Single {
                    name,
                    key: Box::new(Ast::KeyTuple(parts)),
                })
            }
        }
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
        rollup: block.rollup,
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
        Expr::Lit(_) | Expr::Param(_) => e.clone(),
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
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter().map(|a| substitute_aliases(a, aliases)).collect(),
        ),
        Expr::If { cond, then_branch, else_branch } => Expr::If {
            cond: Box::new(substitute_aliases(cond, aliases)),
            then_branch: Box::new(substitute_aliases(then_branch, aliases)),
            else_branch: Box::new(substitute_aliases(else_branch, aliases)),
        },
        // Aggregate-`let` substitution doesn't descend into a subquery:
        // the inner query has its own scope and resolves aliases at its
        // own lowering time, so the macro names don't apply inside it.
        Expr::Subquery(_) => e.clone(),
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
        Expr::Call(name, args) => {
            // Recurse into every argument so reducer calls nested in a
            // function (e.g. `round(sum(.x))`) hoist into the surrounding
            // aggregate block's reduction list.
            let lowered = args
                .iter()
                .map(|a| lower_output_expr(a, reductions, where_pred_ast, item_name, env))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Ast::Call { name: name.clone(), args: lowered })
        }
        Expr::If { cond, then_branch, else_branch } => {
            // Recurse into all three arms so reducer calls anywhere in
            // the conditional hoist into the surrounding aggregate
            // block's reduction list.
            let c = lower_output_expr(cond, reductions, where_pred_ast, item_name, env)?;
            let t = lower_output_expr(then_branch, reductions, where_pred_ast, item_name, env)?;
            let el = lower_output_expr(else_branch, reductions, where_pred_ast, item_name, env)?;
            Ok(Ast::If {
                cond: Box::new(c),
                then_branch: Box::new(t),
                else_branch: Box::new(el),
            })
        }
        // Boolean / comparison wrappers can carry reducer calls when
        // they appear as a sub-expression of an `if(...)` arm. Recurse
        // through them so the hoist reaches the embedded reducers.
        Expr::Compare(l, op, r) => {
            let la = lower_output_expr(l, reductions, where_pred_ast, item_name, env)?;
            let ra = lower_output_expr(r, reductions, where_pred_ast, item_name, env)?;
            Ok(Ast::Compare(Box::new(la), *op, Box::new(ra)))
        }
        Expr::And(l, r) => {
            let la = lower_output_expr(l, reductions, where_pred_ast, item_name, env)?;
            let ra = lower_output_expr(r, reductions, where_pred_ast, item_name, env)?;
            Ok(Ast::And(Box::new(la), Box::new(ra)))
        }
        Expr::Or(l, r) => {
            let la = lower_output_expr(l, reductions, where_pred_ast, item_name, env)?;
            let ra = lower_output_expr(r, reductions, where_pred_ast, item_name, env)?;
            Ok(Ast::Or(Box::new(la), Box::new(ra)))
        }
        Expr::Not(inner) => {
            let i = lower_output_expr(inner, reductions, where_pred_ast, item_name, env)?;
            Ok(pipe(i, Ast::Not))
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
        Expr::Param(name) => match env.params.get(name) {
            Some(v) => Ok(param_to_ast(v)),
            None => Err(QueryError::new(
                0,
                format!("undefined query parameter `${}`", name),
            )),
        },
        Expr::Array(items) => {
            let lowered = items
                .iter()
                .map(|x| lower_expr(x, env))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Ast::ArrayLit(lowered))
        }
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
        Expr::Subquery(q) => {
            let pipeline = lower_query_scoped(q, &env.params, &env.aliases)?;
            Ok(Ast::Subquery { pipeline: Box::new(pipeline) })
        }
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
        Expr::Call(name, args) => {
            let lowered = args
                .iter()
                .map(|a| lower_expr(a, env))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Ast::Call { name: name.clone(), args: lowered })
        }
        Expr::If { cond, then_branch, else_branch } => {
            let c = lower_expr(cond, env)?;
            let t = lower_expr(then_branch, env)?;
            let e = lower_expr(else_branch, env)?;
            Ok(Ast::If {
                cond: Box::new(c),
                then_branch: Box::new(t),
                else_branch: Box::new(e),
            })
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
    // `x in (SUBQUERY)` — membership over the subquery's emission stream.
    // Lowers to "does the subquery emit any row equal to x?": run the
    // inner pipeline, keep only emissions equal to the (outer-correlated)
    // lhs, and test for existence. `not in` negates the existence test.
    if let Expr::Subquery(q) = rhs {
        let lhs_ast = lower_expr(lhs, env)?;
        let pipeline = lower_query_scoped(q, &env.params, &env.aliases)?;
        let member = pipe(
            Ast::Subquery { pipeline: Box::new(pipeline) },
            Ast::Select(Box::new(Ast::Compare(
                Box::new(Ast::Identity),
                CompareOp::Eq,
                Box::new(lhs_ast),
            ))),
        );
        let exists = Ast::Exists(Box::new(member));
        return Ok(if negated {
            pipe(exists, Ast::Not)
        } else {
            exists
        });
    }
    let items = match rhs {
        Expr::Array(xs) => xs,
        _ => {
            return Err(QueryError::new(
                0,
                "right-hand side of `in` must be an array literal or a subquery".into(),
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
        params: HashMap::new(),
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
        Ast::JoinEach { outer_key, lookup, .. } => {
            30u64.saturating_add(predicate_cost(outer_key)).saturating_add(predicate_cost(lookup))
        }
        Ast::UnnestEach { source, .. } => {
            20u64.saturating_add(predicate_cost(source))
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
        Ast::KeyTuple(parts) | Ast::ArrayLit(parts) => parts.iter().map(predicate_cost).sum(),
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
        Ast::Call { args, .. } => args
            .iter()
            .map(predicate_cost)
            .fold(2u64, |acc, c| acc.saturating_add(c)),
        Ast::If { cond, then_branch, else_branch } => 2u64
            .saturating_add(predicate_cost(cond))
            // Take the cheaper of the two branches — at runtime only one
            // is walked, so an expensive `else` arm shouldn't push a
            // predicate to the back of the chain if `then` is cheap.
            .saturating_add(predicate_cost(then_branch).min(predicate_cost(else_branch))),
        // A correlated subquery re-runs an entire inner pipeline per outer
        // row — the most expensive thing a predicate can do. Cost it above
        // an aggregate so `where` reordering pushes it to the back, after
        // every cheaper filter has had a chance to drop the row.
        Ast::Subquery { pipeline } => 2000u64.saturating_add(predicate_cost(pipeline)),
    }
}
