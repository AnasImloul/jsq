//! Query execution surface: parsing, evaluation, results, and the
//! peripheral helpers around query authoring (tokenisation for syntax
//! highlighting, completion classification, formatting, grammar
//! manifest export).

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::document::{Document, NodeKind, NULL_NODE};
use crate::query::evaluator::QueryResult;
use crate::query::evaluator::render as value_render;
use crate::query::value::Value;
use crate::query::{self, evaluator, QueryError};

use super::{push_json_escaped, string_to_owned_bytes, EngineOwnedBytes, EngineSlice};

/// Try `s` as a full surface query first; if that fails, fall back to a
/// bare path expression. The autocomplete helpers (`engine_kinds_for_query`,
/// `engine_keys_for_query`) call this because the completion classifier
/// can hand back either: a full `from … as …` prefix for cursors that
/// sit inside a query, or a bare path for the field-access-against-root
/// case.
fn compile_query_or_path(s: &str) -> Option<query::ast::Ast> {
    if let Ok(a) = query::compile(s) {
        return Some(a);
    }
    query::surface::compile_path_only(s).ok()
}

// ----------------------------------------------------------------------------
// Result handle
// ----------------------------------------------------------------------------

pub struct QueryResults {
    pub results: Vec<QueryResult>,
    pub hit_limit: bool,
    /// `Some` when evaluation hit a `lookup` with no matching index.
    /// The two CStrings hold the canonicalised SOURCE and KEY ASTs so
    /// the UI can echo them back when offering to create the index.
    pub missing_index: Option<(CString, CString)>,
    /// How many rows the source path emitted before the rest of the
    /// pipeline ran. Surfaced in the UI's stats popover so users can
    /// see how much data the query actually walked.
    pub scanned_rows: u64,
    /// Successful `lookup(...)` invocations during this run. Helps
    /// users see when a field-set has fanned out into a high-volume
    /// per-row workload.
    pub lookup_calls: u64,
    /// Sum of source byte spans for every node the source path
    /// emitted. Surfaced in the UI's stats popover so users can spot
    /// memory-bandwidth-bound queries — when this approaches the
    /// file size the engine touched most of the document.
    pub scanned_bytes: u64,
    /// Per-row presentation strings (`(preview, full_text)`) the desktop
    /// table view reads via `engine_query_results_at`. Built eagerly so
    /// the FFI handle owns the bytes for its full lifetime — `_at`
    /// hands out borrowed slices into these strings, so they need a
    /// stable address. The engine row itself (`QueryResult`) carries
    /// only the structured `Value`; everything string-y is derived.
    pub presentation: Vec<RowPresentation>,
}

pub struct RowPresentation {
    pub preview: String,
    pub full_text: String,
}

fn build_presentation(results: &[QueryResult], doc: &Document) -> Vec<RowPresentation> {
    results
        .iter()
        .map(|r| {
            let preview = value_render::value_preview(doc, &r.value, 80);
            // Document-backed containers are rendered lazily by the UI: it
            // fetches their children on demand by node id and never reads
            // full_text. Serializing the whole subtree here would copy the
            // entire node across the FFI/IPC boundary for nothing (seconds on
            // a 90MB row). Only scalars and synthetic values need full_text.
            let is_node_container = matches!(r.value, Value::Node(_))
                && (r.kind == NodeKind::Array as u8 || r.kind == NodeKind::Object as u8);
            let mut full_text = String::new();
            if !is_node_container {
                value_render::write_value_json(&mut full_text, doc, &r.value);
            }
            RowPresentation { preview, full_text }
        })
        .collect()
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct EngineQueryResultView {
    pub node_id: u32,
    pub kind: u8,
    pub _pad: [u8; 3],
    pub path: EngineSlice,
    pub preview: EngineSlice,
    pub full_text: EngineSlice,
}

impl EngineQueryResultView {
    fn empty() -> Self {
        Self {
            node_id: NULL_NODE,
            kind: 0,
            _pad: [0; 3],
            path: EngineSlice::empty(),
            preview: EngineSlice::empty(),
            full_text: EngineSlice::empty(),
        }
    }
}

// ----------------------------------------------------------------------------
// Parse-error reporting (thread-local; shared with [`super::index`])
// ----------------------------------------------------------------------------

thread_local! {
    static LAST_QUERY_ERROR: RefCell<Option<(CString, u32)>> = const { RefCell::new(None) };
}

pub(super) fn set_query_error(err: QueryError) {
    let pos = err.position as u32;
    let msg = CString::new(err.message)
        .unwrap_or_else(|_| CString::new("query error").expect("ASCII"));
    LAST_QUERY_ERROR.with(|cell| *cell.borrow_mut() = Some((msg, pos)));
}

#[no_mangle]
pub extern "C" fn engine_query_last_parse_error() -> *const c_char {
    LAST_QUERY_ERROR.with(|cell| {
        cell.borrow().as_ref().map(|(c, _)| c.as_ptr()).unwrap_or(std::ptr::null())
    })
}

#[no_mangle]
pub extern "C" fn engine_query_last_parse_error_position() -> u32 {
    LAST_QUERY_ERROR.with(|cell| {
        cell.borrow().as_ref().map(|(_, pos)| *pos).unwrap_or(0)
    })
}

// ----------------------------------------------------------------------------
// Query execution
// ----------------------------------------------------------------------------

/// Parses and evaluates the query against the document.
/// Returns NULL on parse error; check engine_query_last_parse_error.
#[no_mangle]
pub extern "C" fn engine_query_run(
    doc: *const Document,
    query: *const c_char,
    limit: u32,
) -> *mut QueryResults {
    let Some(d) = (unsafe { doc.as_ref() }) else { return std::ptr::null_mut() };
    if query.is_null() {
        set_query_error(QueryError::new(0, "null query".into()));
        return std::ptr::null_mut();
    }
    let cstr = unsafe { CStr::from_ptr(query) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_query_error(QueryError::new(0, "non-UTF-8 query".into()));
            return std::ptr::null_mut();
        }
    };
    let ast = match query::compile(s) {
        Ok(a) => a,
        Err(e) => {
            set_query_error(e);
            return std::ptr::null_mut();
        }
    };
    if d.records().is_empty() {
        return std::ptr::null_mut();
    }
    let output = evaluator::run(d, &ast, 0, limit as usize);
    let missing_index = match output.error {
        Some(evaluator::EvalError::MissingIndex { source, key }) => {
            let src = CString::new(source).unwrap_or_else(|_| CString::new("").unwrap());
            let key = CString::new(key).unwrap_or_else(|_| CString::new("").unwrap());
            Some((src, key))
        }
        None => None,
    };
    let presentation = build_presentation(&output.results, d);
    Box::into_raw(Box::new(QueryResults {
        results: output.results,
        hit_limit: output.hit_limit,
        missing_index,
        scanned_rows: output.scanned_rows,
        lookup_calls: output.lookup_calls,
        scanned_bytes: output.scanned_bytes,
        presentation,
    }))
}

/// Plain-text substring search across the entire document. Walks every
/// node and emits any whose object key OR primitive value contains the
/// (case-insensitive ASCII) needle. Bypasses the jq parser entirely so
/// users don't have to know jq syntax — typing a leading `/` in the
/// query bar routes to this.
#[no_mangle]
pub extern "C" fn engine_query_text_search(
    doc: *const Document,
    needle: *const c_char,
    limit: u32,
) -> *mut QueryResults {
    let Some(d) = (unsafe { doc.as_ref() }) else { return std::ptr::null_mut() };
    if needle.is_null() {
        return std::ptr::null_mut();
    }
    let cstr = unsafe { CStr::from_ptr(needle) };
    let bytes = cstr.to_bytes();
    if bytes.is_empty() {
        return std::ptr::null_mut();
    }
    let output = evaluator::text_search(d, bytes, limit as usize);
    let presentation = build_presentation(&output.results, d);
    Box::into_raw(Box::new(QueryResults {
        results: output.results,
        hit_limit: output.hit_limit,
        missing_index: None,
        scanned_rows: output.scanned_rows,
        lookup_calls: output.lookup_calls,
        scanned_bytes: output.scanned_bytes,
        presentation,
    }))
}

#[no_mangle]
pub extern "C" fn engine_query_results_free(results: *mut QueryResults) {
    if results.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(results) });
}

#[no_mangle]
pub extern "C" fn engine_query_results_count(results: *const QueryResults) -> u32 {
    let Some(r) = (unsafe { results.as_ref() }) else { return 0 };
    r.results.len() as u32
}

#[no_mangle]
pub extern "C" fn engine_query_results_hit_limit(results: *const QueryResults) -> u8 {
    let Some(r) = (unsafe { results.as_ref() }) else { return 0 };
    if r.hit_limit { 1 } else { 0 }
}

/// Number of rows the source path emitted before the rest of the
/// pipeline ran. Surfaced in the UI's stats popover. Zero on a null
/// `results` handle.
#[no_mangle]
pub extern "C" fn engine_query_results_scanned_rows(results: *const QueryResults) -> u64 {
    let Some(r) = (unsafe { results.as_ref() }) else { return 0 };
    r.scanned_rows
}

/// Number of `lookup(...)` invocations that ran during this query.
/// Useful for diagnosing when a field-set fanned out into a high
/// per-row workload. Zero on a null `results` handle.
#[no_mangle]
pub extern "C" fn engine_query_results_lookup_calls(results: *const QueryResults) -> u64 {
    let Some(r) = (unsafe { results.as_ref() }) else { return 0 };
    r.lookup_calls
}

/// Total source byte span of nodes the source path emitted during
/// this query. Comparing this against the document's file size tells
/// you whether the query was bandwidth-bound — values near the file
/// size mean the engine read most of the document regardless of how
/// many rows survived downstream filtering. Zero on a null handle.
#[no_mangle]
pub extern "C" fn engine_query_results_scanned_bytes(results: *const QueryResults) -> u64 {
    let Some(r) = (unsafe { results.as_ref() }) else { return 0 };
    r.scanned_bytes
}

#[no_mangle]
pub extern "C" fn engine_query_results_at(
    results: *const QueryResults,
    idx: u32,
) -> EngineQueryResultView {
    let Some(r) = (unsafe { results.as_ref() }) else { return EngineQueryResultView::empty() };
    let Some(item) = r.results.get(idx as usize) else { return EngineQueryResultView::empty() };
    let Some(present) = r.presentation.get(idx as usize) else {
        return EngineQueryResultView::empty();
    };
    // `node_id` is the only UI-side click-through hint. For real
    // document nodes it's the record id; for synthetic rows we surface
    // NULL_NODE so the table view's "has node id" check skips them.
    // GroupList rows used to advertise their first member's id so the
    // inspector could jump into the bucket — preserve that here.
    let node_id = match &item.value {
        Value::Node(id) => *id,
        Value::GroupList { members, .. } => members.first().copied().unwrap_or(NULL_NODE),
        _ => NULL_NODE,
    };
    EngineQueryResultView {
        node_id,
        kind: item.kind,
        _pad: [0; 3],
        path: EngineSlice::from_slice(item.path.as_bytes()),
        preview: EngineSlice::from_slice(present.preview.as_bytes()),
        full_text: EngineSlice::from_slice(present.full_text.as_bytes()),
    }
}

// ----------------------------------------------------------------------------
// Output formatters
// ----------------------------------------------------------------------------

/// Render-format discriminant for `engine_query_run_and_render`.
pub const ENGINE_RENDER_NDJSON: u8 = 0;
pub const ENGINE_RENDER_JSON_ARRAY: u8 = 1;
pub const ENGINE_RENDER_CSV: u8 = 2;

/// One-shot helper that runs `query` against `doc` and immediately
/// renders the result set in the requested format, for callers that
/// don't hold onto a query-results handle long enough to call the
/// per-format renderers separately.
///
/// Returns owned UTF-8 bytes (free with `engine_free_owned_bytes`) or
/// `{NULL, 0}` on parse / lookup error — in the parse-error case the
/// usual `engine_query_last_parse_error*` thread-locals are populated.
#[no_mangle]
pub extern "C" fn engine_query_run_and_render(
    doc: *const Document,
    query: *const c_char,
    limit: u32,
    format: u8,
) -> EngineOwnedBytes {
    let empty = EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    let Some(d) = (unsafe { doc.as_ref() }) else { return empty };
    if query.is_null() {
        return empty;
    }
    let cstr = unsafe { CStr::from_ptr(query) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_query_error(QueryError::new(0, "non-UTF-8 query".into()));
            return empty;
        }
    };
    let ast = match query::compile(s) {
        Ok(a) => a,
        Err(e) => {
            set_query_error(e);
            return empty;
        }
    };
    let output = evaluator::run(d, &ast, 0, limit as usize);
    if output.error.is_some() {
        // Missing index / runtime errors don't surface a renderable
        // result set — return empty bytes; the caller already has the
        // option of running `engine_query_run` to inspect the error.
        return empty;
    }
    let bytes = match format {
        ENGINE_RENDER_NDJSON => crate::render::render_ndjson(&output.results, d),
        ENGINE_RENDER_JSON_ARRAY => crate::render::render_json_array(&output.results, d),
        ENGINE_RENDER_CSV => crate::render::render_csv(&output.results, d),
        _ => return empty,
    };
    string_to_owned_bytes(bytes)
}

/// Returns the canonicalised SOURCE expression of the missing-index
/// error attached to this query result, or NULL if there's no error.
/// Pointer is valid until `engine_query_results_free` is called.
#[no_mangle]
pub extern "C" fn engine_query_results_missing_index_source(
    results: *const QueryResults,
) -> *const c_char {
    let Some(r) = (unsafe { results.as_ref() }) else { return std::ptr::null() };
    match &r.missing_index {
        Some((s, _)) => s.as_ptr(),
        None => std::ptr::null(),
    }
}

/// Returns the canonicalised KEY expression of the missing-index error,
/// or NULL if there's no error.
#[no_mangle]
pub extern "C" fn engine_query_results_missing_index_key(
    results: *const QueryResults,
) -> *const c_char {
    let Some(r) = (unsafe { results.as_ref() }) else { return std::ptr::null() };
    match &r.missing_index {
        Some((_, k)) => k.as_ptr(),
        None => std::ptr::null(),
    }
}

// ----------------------------------------------------------------------------
// Schema-aware completion helpers
// ----------------------------------------------------------------------------

/// Bitmask of the kinds produced by sampling up to `limit` outputs of
/// the query. Bit positions match the ENGINE_KIND_* values:
///   bit 0 = null, 1 = bool, 2 = number, 3 = string, 4 = array, 5 = object.
/// Returns 0 on parse error or no outputs.
#[no_mangle]
pub extern "C" fn engine_kinds_for_query(
    doc: *const Document,
    query: *const c_char,
    limit: u32,
) -> u8 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    if query.is_null() {
        return 0;
    }
    let cstr = unsafe { CStr::from_ptr(query) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let ast = match compile_query_or_path(s) {
        Some(a) => a,
        None => return 0,
    };
    evaluator::collect_kinds(d, &ast, limit as usize)
}

/// Schema-aware autocomplete helper. Runs `query` against the document,
/// collects the union of object keys produced by sampling up to `limit`
/// outputs, and returns them as a JSON array of strings (UTF-8 bytes,
/// owned). Returns {NULL, 0} on parse error or non-object outputs.
#[no_mangle]
pub extern "C" fn engine_keys_for_query(
    doc: *const Document,
    query: *const c_char,
    limit: u32,
) -> EngineOwnedBytes {
    let empty = EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    let Some(d) = (unsafe { doc.as_ref() }) else { return empty };
    if query.is_null() {
        return empty;
    }
    let cstr = unsafe { CStr::from_ptr(query) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return empty,
    };
    let ast = match compile_query_or_path(s) {
        Some(a) => a,
        None => return empty,
    };

    let keys = evaluator::collect_keys(d, &ast, limit as usize);
    if keys.is_empty() {
        return empty;
    }

    let mut json = String::from("[");
    for (i, k) in keys.iter().enumerate() {
        if i > 0 { json.push(','); }
        json.push('"');
        push_json_escaped(&mut json, k);
        json.push('"');
    }
    json.push(']');

    string_to_owned_bytes(json)
}

// ----------------------------------------------------------------------------
// Surface language: grammar manifest, tokeniser, completion, formatter
// ----------------------------------------------------------------------------

/// JSON dump of the surface-language grammar manifest. Stable shape —
/// see `query::grammar::manifest_json`. Cached on the UI side; the
/// process-lifetime contract is "Rust is the source of truth, the UI
/// reads it once".
#[no_mangle]
pub extern "C" fn engine_grammar_manifest() -> EngineOwnedBytes {
    string_to_owned_bytes(crate::query::grammar::manifest_json())
}

/// Tokenises `source` for the UI highlighter. Returns a JSON array of
/// `{"category": "...", "offset": N, "length": M}` triples, where
/// offset and length are in **UTF-16 code units** to match JavaScript's
/// string indexing (`String.length` / `.slice`).
///
/// Forgiving — never fails. Unrecognised bytes surface as `error`
/// tokens, malformed strings as a single `string` token to EOF, etc.
/// The UI highlighter walks this list per keystroke and maps each
/// `category` to a colour.
///
/// Returns `{NULL, 0}` only if `source` is null or non-UTF-8.
#[no_mangle]
pub extern "C" fn engine_tokenize(source: *const c_char) -> EngineOwnedBytes {
    let empty = EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    if source.is_null() {
        return empty;
    }
    let cstr = unsafe { CStr::from_ptr(source) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return empty,
    };

    let tokens = crate::query::lexer::tokenize_for_ui(s);
    let mut out = String::with_capacity(tokens.len() * 48 + 2);
    out.push('[');
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"category\":\"");
        out.push_str(t.category.as_str());
        out.push_str("\",\"offset\":");
        out.push_str(&t.offset.to_string());
        out.push_str(",\"length\":");
        out.push_str(&t.length.to_string());
        out.push('}');
    }
    out.push(']');
    string_to_owned_bytes(out)
}

/// Cursor-aware autocomplete classifier. Given `(source, cursor_utf16)`,
/// returns a JSON object describing what kind of completion makes sense
/// at the cursor:
///
/// ```json
/// {
///   "mode": "fieldAccess" | "valueStart" | "afterExpression",
///   "partial": "...",
///   "partialUtf16Length": N,
///   "contextQuery": "..."   // only when mode == "fieldAccess"
/// }
/// ```
///
/// `cursor_utf16` is in UTF-16 code units (JS string-index compatible). Returns
/// `{NULL, 0}` when the cursor is in a position that doesn't admit
/// completions (e.g. mid-token after a number). On null / non-UTF-8
/// `source` also returns `{NULL, 0}`.
///
/// Caller-owned bytes; pair with `engine_free_owned_bytes`.
#[no_mangle]
pub extern "C" fn engine_completion_context(
    source: *const c_char,
    cursor_utf16: u32,
) -> EngineOwnedBytes {
    let empty = EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    if source.is_null() {
        return empty;
    }
    let cstr = unsafe { CStr::from_ptr(source) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => return empty,
    };
    let Some(ctx) = crate::query::surface::completion::classify(s, cursor_utf16) else {
        return empty;
    };

    let mut out = String::with_capacity(ctx.partial.len() + 128);
    out.push_str("{\"mode\":\"");
    out.push_str(crate::query::surface::completion::mode_str(&ctx.mode));
    out.push_str("\",\"partial\":\"");
    push_json_escaped(&mut out, &ctx.partial);
    out.push_str("\",\"partialUtf16Length\":");
    out.push_str(&ctx.partial_utf16_length.to_string());
    if let Some(q) = &ctx.context_query {
        out.push_str(",\"contextQuery\":\"");
        push_json_escaped(&mut out, q);
        out.push('"');
    }
    out.push('}');
    string_to_owned_bytes(out)
}

/// Re-formats a query string with canonical indentation. Returns
/// `{NULL, 0}` on parse error — the caller can read
/// `engine_query_last_parse_error()` to surface the message. The
/// returned bytes are owned and must be freed with
/// `engine_free_owned_bytes`.
#[no_mangle]
pub extern "C" fn engine_format_query(query: *const c_char) -> EngineOwnedBytes {
    let empty = EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    if query.is_null() {
        set_query_error(QueryError::new(0, "null query".into()));
        return empty;
    }
    let cstr = unsafe { CStr::from_ptr(query) };
    let s = match cstr.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_query_error(QueryError::new(0, "non-UTF-8 query".into()));
            return empty;
        }
    };
    let formatted = match crate::query::surface::format(s) {
        Ok(s) => s,
        Err(e) => {
            set_query_error(e);
            return empty;
        }
    };
    string_to_owned_bytes(formatted)
}
