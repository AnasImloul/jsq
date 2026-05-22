//! Query subsystem: lexer + surface parser + evaluator. The surface
//! language is the single user-facing dialect; everything compiles
//! through `query::compile` to the engine's `Ast` and then the
//! evaluator. Operates against the engine's index directly — no
//! JSONNode wrappers in the iteration loop.

pub mod ast;
pub mod evaluator;
pub mod grammar;
pub mod index;
pub mod lexer;
pub mod surface;
pub mod value;

#[derive(Clone, Debug)]
pub struct QueryError {
    pub message: String,
    pub position: usize,
}

impl QueryError {
    pub fn new(position: usize, message: String) -> Self {
        Self { message, position }
    }
}

/// Compiles surface-language source to an executable `Ast`. Single
/// entry point for everything that takes a user-typed query — the
/// query-bar FFI, autocomplete sampling, and path-shaped inputs to
/// index registration. A bare path is itself a valid surface query
/// and round-trips through `Ast::Display` to the same canonical form
/// the index registry keys on, so a path can be both queried and
/// indexed without separate parsing paths.
pub fn compile(source: &str) -> Result<ast::Ast, QueryError> {
    surface::compile(source)
}
