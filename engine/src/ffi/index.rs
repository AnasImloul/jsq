//! Foreign-key index management. Indexes are keyed on the canonical
//! string form of `(source_expr, key_expr)` ASTs and cached on the
//! [`Document`]; subsequent `lookup(...)` calls resolve in O(1).

use std::os::raw::c_char;

use crate::document::Document;
use crate::query;

use super::cstr_to_str;
use super::query::set_query_error;

#[repr(C)]
pub struct EngineIndexStats {
    /// 1 on success (parsed both expressions and built the index), 0 on
    /// failure (parse error — check engine_query_last_parse_error).
    pub ok: u8,
    pub _pad: [u8; 7],
    /// Total source items the build saw (including those whose key was
    /// missing or non-scalar).
    pub source_count: u64,
    /// Items that produced a usable scalar key and were bucketed.
    pub indexed_count: u64,
    /// Distinct key values — i.e. the number of buckets in the resulting
    /// hashmap.
    pub bucket_count: u64,
    /// Approximate retained heap, for the UI's memory-cost hint.
    pub approx_bytes: u64,
}

impl EngineIndexStats {
    fn failure() -> Self {
        Self {
            ok: 0,
            _pad: [0; 7],
            source_count: 0,
            indexed_count: 0,
            bucket_count: 0,
            approx_bytes: 0,
        }
    }
}

/// Builds and registers a foreign-key index on `(source_expr, key_expr)`.
/// Both expressions are parsed as ordinary jq filters; the index then maps
/// each source item's key value (extracted by `key_expr`) to its node ID,
/// so subsequent `lookup(source_expr; key_expr)` calls become O(1).
///
/// Re-running with the same expressions rebuilds the index in place
/// (useful if the document has been re-opened with different state).
#[no_mangle]
pub extern "C" fn engine_query_create_index(
    doc: *const Document,
    source_expr: *const c_char,
    key_expr: *const c_char,
) -> EngineIndexStats {
    let Some(d) = (unsafe { doc.as_ref() }) else { return EngineIndexStats::failure() };
    let Some(src_str) = (unsafe { cstr_to_str(source_expr) }) else { return EngineIndexStats::failure() };
    let Some(key_str) = (unsafe { cstr_to_str(key_expr) }) else { return EngineIndexStats::failure() };

    // Source / key are bare path expressions — the canonical form the
    // join-lowering canonicalises and the registry hashes on. Use the
    // path-only compiler since the SQL-shaped surface parser requires
    // a full `from … as …` query.
    let source_ast = match query::surface::compile_path_only(src_str) {
        Ok(a) => a,
        Err(e) => { set_query_error(e); return EngineIndexStats::failure(); }
    };
    let key_ast = match query::surface::compile_path_only(key_str) {
        Ok(a) => a,
        Err(e) => { set_query_error(e); return EngineIndexStats::failure(); }
    };

    let source_canon = source_ast.to_string();
    let key_canon = key_ast.to_string();

    let index = crate::query::index::ForeignKeyIndex::build(d, &source_ast, &key_ast);
    let stats = EngineIndexStats {
        ok: 1,
        _pad: [0; 7],
        source_count: index.source_count as u64,
        indexed_count: index.indexed_count as u64,
        bucket_count: index.buckets.len() as u64,
        approx_bytes: index.approx_bytes() as u64,
    };

    if let Ok(mut reg) = d.indexes.lock() {
        reg.insert(source_canon, key_canon, index);
    }
    stats
}

