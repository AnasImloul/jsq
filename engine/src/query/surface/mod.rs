//! Surface query language — the user-facing SQL-shaped syntax.
//! Compiles to the engine's `query::ast::Ast` via `lower::lower_query`.
//! Clauses: `(fields …)* from … as … (join …)* (where …)? (let …)?
//! distinct? (aggregate { … } | collect by …)? (select …)?
//! (order by …)? (limit N)?`.

pub mod ast;
pub mod completion;
pub mod format;
pub mod lower;
pub mod parser;

use super::ast::Ast;
use super::QueryError;
use std::collections::HashMap;

pub use lower::ParamValue;

/// Parse + lower in one shot. The returned `Ast` is consumable by the
/// evaluator and uses canonical lookup forms — `Lookup` shapes produced
/// here round-trip through `Ast::Display` to the same strings the
/// index registry keys on, so an already-built foreign-key index
/// services queries that reference it without rebuild.
pub fn compile(source: &str) -> Result<Ast, QueryError> {
    compile_with_params(source, &HashMap::new())
}

/// Like [`compile`], but substitutes `$name` query parameters from
/// `params`. An unbound parameter referenced by the query is a compile
/// error.
pub fn compile_with_params(
    source: &str,
    params: &HashMap<String, ParamValue>,
) -> Result<Ast, QueryError> {
    let query = parser::parse(source)?;
    lower::lower_query(query, params)
}

/// Re-emits `source` as a canonically-formatted query.
pub fn format(source: &str) -> Result<String, QueryError> {
    let query = parser::parse(source)?;
    Ok(format::format_query(&query))
}

/// Parses + lowers a bare path expression (no clauses) into an `Ast`.
/// Used by index builders that need to canonicalize a path string —
/// e.g. registering a `ForeignKeyIndex` whose source/key match the
/// canonical forms a `join` clause produces.
pub fn compile_path_only(source: &str) -> Result<Ast, QueryError> {
    let path = parser::parse_path_only(source)?;
    lower::lower_path_only(&path)
}
