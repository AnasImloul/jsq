//! Tauri command layer. Thin handlers that lock the open document and
//! delegate to the safe `bridge` wrappers.

use std::path::PathBuf;

use tauri::State;

use crate::bridge::{self, DocHandle};
use crate::dto::{
    AncestorDto, ChildDto, FieldSchema, NodeDetailDto, OpenResult, QueryRunDto,
};
use crate::state::AppState;

fn with_doc<T>(
    state: &State<AppState>,
    doc_id: u32,
    f: impl FnOnce(&DocHandle) -> Result<T, String>,
) -> Result<T, String> {
    let guard = state.docs.lock().map_err(|_| "state lock poisoned".to_string())?;
    let doc = guard.get(doc_id).ok_or_else(|| "no document open".to_string())?;
    f(doc)
}

#[tauri::command(async)]
pub fn open(state: State<AppState>, path: String) -> Result<OpenResult, String> {
    let source = PathBuf::from(&path);
    let index_dir = cache_dir();
    let handle = DocHandle::open(&source, index_dir.as_deref())?;
    let mut guard = state.docs.lock().map_err(|_| "state lock poisoned".to_string())?;
    let id = guard.insert(handle);
    Ok(guard.get(id).expect("just inserted").meta(id))
}

#[tauri::command]
pub fn close(state: State<AppState>, doc: u32) -> Result<(), String> {
    state
        .docs
        .lock()
        .map_err(|_| "state lock poisoned".to_string())?
        .remove(doc);
    Ok(())
}

/// Current parse progress as `[parsed, total]` byte counts. `total` is
/// zero before any parse starts; polled by the loading screen while an
/// `open` is in flight on another worker thread.
#[tauri::command]
pub fn parse_progress() -> (u64, u64) {
    let mut parsed: u64 = 0;
    let mut total: u64 = 0;
    engine::ffi::engine_current_parse_progress(&mut parsed, &mut total);
    (parsed, total)
}

#[tauri::command(async)]
pub fn children(
    state: State<AppState>,
    doc: u32,
    parent: u32,
    offset: u32,
    limit: u32,
) -> Result<Vec<ChildDto>, String> {
    with_doc(&state, doc, |d| Ok(d.children(parent, offset, limit)))
}

#[tauri::command(async)]
pub fn node_value(state: State<AppState>, doc: u32, node: u32) -> Result<Option<String>, String> {
    with_doc(&state, doc, |d| Ok(d.node_value(node)))
}

#[tauri::command(async)]
pub fn node_path(state: State<AppState>, doc: u32, node: u32) -> Result<String, String> {
    with_doc(&state, doc, |d| Ok(d.node_path(node)))
}

#[tauri::command(async)]
pub fn node_detail(state: State<AppState>, doc: u32, node: u32) -> Result<NodeDetailDto, String> {
    with_doc(&state, doc, |d| Ok(d.node_detail(node)))
}

#[tauri::command(async)]
pub fn node_ancestors(
    state: State<AppState>,
    doc: u32,
    node: u32,
) -> Result<Vec<AncestorDto>, String> {
    with_doc(&state, doc, |d| Ok(d.ancestors(node)))
}

#[tauri::command(async)]
pub fn run_query(
    state: State<AppState>,
    doc: u32,
    query: String,
    limit: u32,
) -> Result<QueryRunDto, String> {
    with_doc(&state, doc, |d| d.run_query(&query, limit))
}

#[tauri::command(async)]
pub fn text_search(
    state: State<AppState>,
    doc: u32,
    needle: String,
    limit: u32,
) -> Result<QueryRunDto, String> {
    with_doc(&state, doc, |d| d.text_search(&needle, limit))
}

#[tauri::command(async)]
pub fn render_query(
    state: State<AppState>,
    doc: u32,
    query: String,
    limit: u32,
    format: String,
) -> Result<String, String> {
    let fmt = match format.as_str() {
        "ndjson" => bridge::RENDER_NDJSON,
        "json" => bridge::RENDER_JSON_ARRAY,
        "csv" => bridge::RENDER_CSV,
        other => return Err(format!("unknown export format: {other}")),
    };
    with_doc(&state, doc, |d| d.render(&query, limit, fmt))
}

/// Re-runs `query` and writes the rendered bytes to `path`. The render
/// goes through `engine::render` (same path as the CLI and the in-app
/// preview), so format logic stays single-source. The on-screen results
/// handle is long gone by export time, but the source is page-cached so
/// the second pass is cheap.
#[tauri::command(async)]
pub fn export_query(
    state: State<AppState>,
    doc: u32,
    query: String,
    limit: u32,
    format: String,
    path: String,
) -> Result<(), String> {
    let fmt = match format.as_str() {
        "ndjson" => bridge::RENDER_NDJSON,
        "json" => bridge::RENDER_JSON_ARRAY,
        "csv" => bridge::RENDER_CSV,
        other => return Err(format!("unknown export format: {other}")),
    };
    let rendered = with_doc(&state, doc, |d| d.render(&query, limit, fmt))?;
    std::fs::write(&path, rendered).map_err(|e| format!("write failed: {e}"))
}

#[tauri::command(async)]
pub fn query_schema(
    state: State<AppState>,
    doc: u32,
    query: String,
    limit: u32,
) -> Result<FieldSchema, String> {
    with_doc(&state, doc, |d| {
        Ok(FieldSchema {
            kinds: d.query_kinds(&query, limit),
            keys: d.query_keys(&query, limit),
        })
    })
}

#[tauri::command]
pub fn format_query(query: String) -> Result<String, String> {
    bridge::format_query(&query)
}

#[tauri::command]
pub fn tokenize(source: String) -> String {
    bridge::tokenize(&source)
}

#[tauri::command]
pub fn completion_context(source: String, cursor: u32) -> Option<String> {
    bridge::completion_context(&source, cursor)
}

#[tauri::command]
pub fn grammar_manifest() -> String {
    bridge::grammar_manifest()
}

#[tauri::command]
pub fn engine_version() -> String {
    let p = engine::ffi::engine_version();
    if p.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
}

/// Per-user cache directory for the engine's offset-index sidecars.
fn cache_dir() -> Option<PathBuf> {
    let base = dirs_cache()?;
    let dir = base.join("BigJSON").join("indexes");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn dirs_cache() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Caches"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
}
