//! Foreign-key index management. Indexes are keyed on the canonical
//! string form of `(source_expr, key_expr)` ASTs and cached on the
//! [`Document`]; subsequent `lookup(...)` calls resolve in O(1).

use std::os::raw::c_char;

use crate::document::Document;
use crate::query;

use super::query::set_query_error;
use super::{cstr_to_str, push_json_escaped, string_to_owned_bytes, EngineOwnedBytes};

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

/// Drops the index registered for `(source_expr, key_expr)` if any.
/// Returns 1 if an index was removed, 0 otherwise. Both arguments must
/// match the *canonical* form of the expressions — typically supplied
/// by `engine_query_list_indexes` rather than typed by the user.
#[no_mangle]
pub extern "C" fn engine_query_drop_index(
    doc: *const Document,
    source_canon: *const c_char,
    key_canon: *const c_char,
) -> u8 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    let Some(src) = (unsafe { cstr_to_str(source_canon) }) else { return 0 };
    let Some(key) = (unsafe { cstr_to_str(key_canon) }) else { return 0 };
    if let Ok(mut reg) = d.indexes.lock() {
        if reg.remove(src, key) { 1 } else { 0 }
    } else {
        0
    }
}

/// Returns a JSON array of the registered indexes, each shaped like
/// `{"source": "...", "key": "...", "source_count": N, "indexed_count": M, "bucket_count": K, "approx_bytes": B}`.
/// Caller owns the returned bytes — free with `engine_free_owned_bytes`.
#[no_mangle]
pub extern "C" fn engine_query_list_indexes(doc: *const Document) -> EngineOwnedBytes {
    let empty = EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    let Some(d) = (unsafe { doc.as_ref() }) else { return empty };
    let reg = match d.indexes.lock() {
        Ok(g) => g,
        Err(_) => return empty,
    };
    let entries = reg.list();
    let mut json = String::from("[");
    for (i, (src, key, idx)) in entries.iter().enumerate() {
        if i > 0 { json.push(','); }
        json.push_str("{\"source\":\"");
        push_json_escaped(&mut json, src);
        json.push_str("\",\"key\":\"");
        push_json_escaped(&mut json, key);
        json.push_str("\",\"source_count\":");
        json.push_str(&idx.source_count.to_string());
        json.push_str(",\"indexed_count\":");
        json.push_str(&idx.indexed_count.to_string());
        json.push_str(",\"bucket_count\":");
        json.push_str(&idx.buckets.len().to_string());
        json.push_str(",\"approx_bytes\":");
        json.push_str(&idx.approx_bytes().to_string());
        json.push('}');
    }
    json.push(']');
    string_to_owned_bytes(json)
}
