//! C ABI surface. Every function is `extern "C"` and `#[no_mangle]`.
//! Functions returning slices into the document hand out pointers that are
//! valid for the document's lifetime; functions returning owned bytes give
//! the caller ownership and must be paired with `engine_free_owned_bytes`.
//!
//! Under the hybrid emit-gate, primitives don't have records — they're
//! addressable only through the inline fields of `EngineChildMeta` (rows
//! with `id == NULL_NODE`). `engine_node_*` calls that take a u32 id
//! still operate only on record-bearing nodes. Tree-rendering callers
//! should drive enumeration through `engine_node_children_meta_batch`,
//! which interleaves both kinds of children with full per-row metadata.
//!
//! This module is split into submodules by concern:
//! - [`node`]: per-node accessors (kind, parent, byte ranges, key/value bytes, path).
//! - [`children`]: child enumeration (`EngineChildMeta`, `EngineScanState`, batch APIs).
//! - [`query`]: query execution, results, tokenisation, completion, formatting.
//! - [`index`]: foreign-key index management.
//!
//! The C ABI symbol table is flat regardless of module nesting; this file
//! holds only the shared types, internal helpers, and document lifecycle.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::sync::OnceLock;

use crate::document::{Document, NULL_NODE};
use crate::error::{last_error_ptr, set_last_error, EngineError};

pub mod children;
pub mod index;
pub mod node;
pub mod query;

// Flat re-exports so external Rust callers (tests, benches, examples)
// can keep using `engine::ffi::Foo` regardless of which submodule
// owns the symbol. The C ABI is already flat at the link level; this
// just mirrors that for Rust consumers.
pub use children::{
    engine_node_children_batch, engine_node_children_kind_counts,
    engine_node_children_meta_batch, engine_node_children_meta_batch_resume, EngineChildMeta,
    EngineScanState, ENGINE_SCAN_STATE_FRESH,
};
pub use index::{
    engine_query_create_index, engine_query_drop_index, engine_query_list_indexes,
    EngineIndexStats,
};
pub use node::{
    engine_node_array_index, engine_node_byte_length, engine_node_byte_offset,
    engine_node_child_count, engine_node_first_child, engine_node_is_array_element,
    engine_node_is_object_member, engine_node_key, engine_node_kind, engine_node_next_sibling,
    engine_node_parent, engine_node_path, engine_node_value_bytes, engine_node_value_bytes_at,
};
pub use query::{
    engine_completion_context, engine_format_query, engine_grammar_manifest,
    engine_keys_for_query, engine_kinds_for_query, engine_query_last_parse_error,
    engine_query_last_parse_error_position, engine_query_results_at, engine_query_results_count,
    engine_query_results_free, engine_query_results_hit_limit,
    engine_query_results_lookup_calls, engine_query_results_missing_index_key,
    engine_query_results_missing_index_source, engine_query_results_scanned_bytes,
    engine_query_results_scanned_rows, engine_query_run, engine_query_text_search,
    engine_query_run_and_render, engine_render_csv, engine_render_json_array,
    engine_render_ndjson, engine_tokenize, EngineQueryResultView, QueryResults,
};

// ----------------------------------------------------------------------------
// Shared types
// ----------------------------------------------------------------------------

#[repr(C)]
#[derive(Copy, Clone)]
pub struct EngineSlice {
    pub data: *const u8,
    /// `u64` so multi-GB spans (e.g. the root container of a 10GB
    /// document) round-trip through the borrowed-slice API without
    /// truncation.
    pub length: u64,
}

impl EngineSlice {
    pub(super) fn empty() -> Self {
        Self { data: std::ptr::null(), length: 0 }
    }

    pub(super) fn from_slice(s: &[u8]) -> Self {
        Self { data: s.as_ptr(), length: s.len() as u64 }
    }
}

#[repr(C)]
pub struct EngineOwnedBytes {
    pub data: *mut u8,
    /// `u64` because `engine_free_owned_bytes` reconstructs the slice
    /// with this length to hand back to the global allocator. A `u32`
    /// truncated for a >4 GiB buffer (large query result, big jq path)
    /// would free with a layout mismatched against the original
    /// `Box<[u8]>` allocation — undefined behaviour in the allocator.
    pub length: u64,
}

// ----------------------------------------------------------------------------
// Internal helpers (visible to submodules)
// ----------------------------------------------------------------------------

pub(super) unsafe fn cstr_to_str<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() { return None; }
    CStr::from_ptr(p).to_str().ok()
}

/// Turn an owned `String` into a heap buffer paired with its length, in
/// the shape `EngineOwnedBytes` describes. The caller must free the
/// returned buffer with `engine_free_owned_bytes`.
pub(super) fn string_to_owned_bytes(s: String) -> EngineOwnedBytes {
    let bytes = s.into_bytes();
    let length = bytes.len() as u64;
    let mut boxed = bytes.into_boxed_slice();
    let ptr = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    EngineOwnedBytes { data: ptr, length }
}

pub(super) fn push_json_escaped(out: &mut String, s: &str) {
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
}

// ----------------------------------------------------------------------------
// Document lifecycle / global accessors
// ----------------------------------------------------------------------------

static VERSION_C: OnceLock<CString> = OnceLock::new();

#[no_mangle]
pub extern "C" fn engine_version() -> *const c_char {
    VERSION_C
        .get_or_init(|| {
            CString::new(env!("CARGO_PKG_VERSION")).expect("version contains no NUL")
        })
        .as_ptr()
}

#[no_mangle]
pub extern "C" fn engine_open(
    source_path: *const c_char,
    index_dir: *const c_char,
) -> *mut Document {
    if source_path.is_null() {
        set_last_error(&EngineError::Io("null source path".into()));
        return std::ptr::null_mut();
    }
    let path_cstr = unsafe { CStr::from_ptr(source_path) };
    let source = match path_cstr.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(&EngineError::Io(format!("non-UTF-8 source path: {}", e)));
            return std::ptr::null_mut();
        }
    };
    let dir: Option<&Path> = if index_dir.is_null() {
        None
    } else {
        let dir_cstr = unsafe { CStr::from_ptr(index_dir) };
        match dir_cstr.to_str() {
            Ok(s) if s.is_empty() => None,
            Ok(s) => Some(Path::new(s)),
            Err(e) => {
                set_last_error(&EngineError::Io(format!("non-UTF-8 index dir: {}", e)));
                return std::ptr::null_mut();
            }
        }
    };

    match Document::open(Path::new(source), dir) {
        Ok(doc) => Box::into_raw(Box::new(doc)),
        Err(e) => {
            set_last_error(&e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn engine_loaded_from_sidecar(doc: *const Document) -> u8 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    if d.loaded_from_sidecar() { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn engine_close(doc: *mut Document) {
    if doc.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(doc) });
}

#[no_mangle]
pub extern "C" fn engine_last_error() -> *const c_char {
    last_error_ptr()
}

#[no_mangle]
pub extern "C" fn engine_total_node_count(doc: *const Document) -> u64 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    d.records().len() as u64
}

#[no_mangle]
pub extern "C" fn engine_file_size(doc: *const Document) -> u64 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    d.source_mmap.len() as u64
}

#[no_mangle]
pub extern "C" fn engine_root(doc: *const Document) -> u32 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return NULL_NODE };
    if d.records().is_empty() { NULL_NODE } else { 0 }
}

#[no_mangle]
pub extern "C" fn engine_free_owned_bytes(bytes: EngineOwnedBytes) {
    if bytes.data.is_null() || bytes.length == 0 {
        return;
    }
    unsafe {
        let _ = Box::from_raw(std::slice::from_raw_parts_mut(
            bytes.data,
            bytes.length as usize,
        ));
    }
}

/// Returns the current parse progress as `(parsed, total)` byte
/// counts. `total` is zero before any document has started loading.
/// While a `Document::open` is in flight on another thread, polling
/// this gives a determinate fraction; after the open finishes,
/// `parsed == total`. UI side typically polls every ~100 ms.
#[no_mangle]
pub extern "C" fn engine_current_parse_progress(parsed: *mut u64, total: *mut u64) {
    let (p, t) = crate::progress::current_progress();
    if !parsed.is_null() {
        unsafe { *parsed = p };
    }
    if !total.is_null() {
        unsafe { *total = t };
    }
}
