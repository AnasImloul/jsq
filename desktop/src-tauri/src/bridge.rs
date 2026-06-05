//! Safe Rust wrappers over the `engine` crate's flat FFI surface. The
//! unsafe pointer handling is contained here so the rest of the desktop
//! backend works with ordinary Rust types.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::path::Path;

use engine::document::Document;
use engine::ffi::{
    self, engine_node_value_bytes, engine_node_value_bytes_at, EngineChildMeta, EngineOwnedBytes,
    EngineQueryResultView, EngineScanState, EngineSlice, QueryResults, ENGINE_SCAN_STATE_FRESH,
};

use crate::dto::{
    AncestorDto, ChildDto, NodeDetailDto, OpenResult, QueryRunDto, RowDto, TableCellDto, TableRowDto,
    TableSnapshotDto,
};

/// Cap on surfaced table columns.
const MAX_COLUMNS: usize = 20;

pub const NULL_NODE: u32 = u32::MAX;

const KIND_ARRAY: u8 = 4;
const KIND_OBJECT: u8 = 5;
const FLAG_OBJECT_MEMBER: u8 = 0x01;
const FLAG_ARRAY_ELEMENT: u8 = 0x02;
const FLAG_KEY_IN_SOURCE: u8 = 0x04;

const PREVIEW_MAX_BYTES: u64 = 256;

/// Render-format discriminants, mirrored from `engine::ffi::query`.
pub const RENDER_NDJSON: u8 = 0;
pub const RENDER_JSON_ARRAY: u8 = 1;
pub const RENDER_CSV: u8 = 2;

/// A resumable scan position into a container's children, so successive
/// pages skip the offset form's re-scan-from-zero. `next_offset` is the
/// child index this cursor is poised to return next; a `children` call
/// whose `offset` matches it resumes in O(page) instead of O(offset).
struct ChildCursor {
    next_offset: u32,
    state: EngineScanState,
}

fn fresh_scan_state() -> EngineScanState {
    EngineScanState {
        pos: ENGINE_SCAN_STATE_FRESH,
        next_skippable: NULL_NODE,
        array_index: 0,
    }
}

/// Owns an engine document handle and closes it on drop. The engine is
/// read-only once parsed, so the raw pointer is safe to send across
/// threads behind the app-state mutex.
pub struct DocHandle {
    ptr: *mut Document,
    /// Per-parent pagination cursors, keyed by parent node id. Lets
    /// sequential tree paging through huge containers stay O(N) total
    /// instead of O(N²). Guarded by `RefCell`; every access happens
    /// under the app-state mutex, so there is never concurrent borrow.
    cursors: RefCell<HashMap<u32, ChildCursor>>,
}

unsafe impl Send for DocHandle {}

impl Drop for DocHandle {
    fn drop(&mut self) {
        ffi::engine_close(self.ptr)
    }
}

impl DocHandle {
    pub fn open(source: &Path, index_dir: Option<&Path>) -> Result<Self, String> {
        let src = path_to_cstring(source)?;
        let dir = match index_dir {
            Some(p) => Some(path_to_cstring(p)?),
            None => None,
        };
        let dir_ptr = dir.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        let ptr = ffi::engine_open(src.as_ptr(), dir_ptr);
        if ptr.is_null() {
            return Err(last_error());
        }
        Ok(Self { ptr, cursors: RefCell::new(HashMap::new()) })
    }

    fn as_const(&self) -> *const Document {
        self.ptr as *const Document
    }

    pub fn meta(&self, doc_id: u32) -> OpenResult {
        let p = self.as_const();
        OpenResult {
            doc_id,
            file_size: ffi::engine_file_size(p),
            total_node_count: ffi::engine_total_node_count(p),
            root_id: ffi::engine_root(p),
            loaded_from_sidecar: ffi::engine_loaded_from_sidecar(p) != 0,
        }
    }

    /// One level of `parent`'s children, `[offset, offset+limit)`.
    ///
    /// Drives the engine's resumable scan cursor: when `offset` is the
    /// continuation of the last page served for `parent` (the tree
    /// view's only access pattern) the walk resumes in O(page); a cold
    /// or out-of-order `offset` seeds a fresh cursor, fast-forwarding to
    /// `offset` only when necessary.
    pub fn children(&self, parent: u32, offset: u32, limit: u32) -> Vec<ChildDto> {
        if limit == 0 {
            return Vec::new();
        }
        let p = self.as_const();
        let mut cursors = self.cursors.borrow_mut();

        let resume = cursors.get(&parent).is_some_and(|c| c.next_offset == offset);
        if !resume {
            let mut state = fresh_scan_state();
            // Cold start is normally at offset 0; a non-zero cold offset
            // (e.g. cursor never seeded for this parent) fast-forwards by
            // discarding `offset` children so the cursor lands correctly.
            if offset > 0 {
                self.skip_children(p, parent, &mut state, offset);
            }
            cursors.insert(parent, ChildCursor { next_offset: offset, state });
        }
        let cursor = cursors.get_mut(&parent).expect("cursor seeded above");

        let mut buf: Vec<EngineChildMeta> = vec![zeroed_meta(); limit as usize];
        let written = ffi::engine_node_children_meta_batch_resume(
            p,
            parent,
            &mut cursor.state,
            limit,
            buf.as_mut_ptr(),
        ) as usize;
        cursor.next_offset = offset + written as u32;
        drop(cursors);

        buf.truncate(written);
        buf.iter().map(|m| self.child_dto(m)).collect()
    }

    /// Advances `state` past `count` children without materialising them,
    /// letting a cold cursor resume mid-container. O(count), but only the
    /// rare out-of-order access path reaches it.
    fn skip_children(&self, p: *const Document, parent: u32, state: &mut EngineScanState, count: u32) {
        const CHUNK: u32 = 256;
        let mut scratch: Vec<EngineChildMeta> = vec![zeroed_meta(); CHUNK as usize];
        let mut remaining = count;
        while remaining > 0 {
            let take = remaining.min(CHUNK);
            let got =
                ffi::engine_node_children_meta_batch_resume(p, parent, state, take, scratch.as_mut_ptr());
            if got == 0 {
                break;
            }
            remaining -= got;
        }
    }

    fn child_dto(&self, m: &EngineChildMeta) -> ChildDto {
        let is_container = m.kind == KIND_ARRAY || m.kind == KIND_OBJECT;
        let id = if m.id == NULL_NODE { None } else { Some(m.id) };

        let key = if (m.flags & FLAG_OBJECT_MEMBER) != 0 && m.key_length > 0 {
            self.decode_key(m)
        } else {
            None
        };

        let index = if (m.flags & FLAG_ARRAY_ELEMENT) != 0 {
            Some(m.array_index)
        } else {
            None
        };

        let (preview, truncated) = if is_container {
            (String::new(), false)
        } else {
            self.value_prefix(m.value_offset, m.value_length, PREVIEW_MAX_BYTES)
        };

        ChildDto {
            id,
            kind: m.kind,
            key,
            index,
            child_count: m.child_count,
            is_container,
            preview,
            truncated,
        }
    }

    fn decode_key(&self, m: &EngineChildMeta) -> Option<String> {
        let p = self.as_const();
        let in_source = (m.flags & FLAG_KEY_IN_SOURCE) != 0;
        let slice = engine_node_value_bytes_at(
            p,
            m.key_offset,
            m.key_length as u64,
            if in_source { 1 } else { 0 },
        );
        if in_source {
            // Raw inter-quote source bytes — run through the engine's
            // JSON string decoder.
            let owned = ffi::node::engine_decode_json_string(slice.data, slice.length);
            unsafe { owned_to_string(owned) }
        } else {
            unsafe { slice_to_string(slice) }
        }
    }

    /// Full decoded value text for a record-bearing node (fat string or
    /// container source span), used by the inspector.
    pub fn node_value(&self, node: u32) -> Option<String> {
        let slice = engine_node_value_bytes(self.as_const(), node);
        unsafe { slice_to_string(slice) }
    }

    /// Bounded value text starting at a source offset.
    fn value_prefix(&self, offset: u64, length: u64, max_bytes: u64) -> (String, bool) {
        if length == 0 {
            return (String::new(), false);
        }
        let n = length.min(max_bytes);
        let slice = engine_node_value_bytes_at(self.as_const(), offset, n, 1);
        let text = unsafe { slice_to_string(slice) }.unwrap_or_default();
        (text, n < length)
    }

    pub fn node_path(&self, node: u32) -> String {
        let owned = ffi::engine_node_path(self.as_const(), node);
        unsafe { owned_to_string(owned) }.unwrap_or_else(|| ".".to_string())
    }

    /// Decoded object-member key for a record-bearing node, if it has one.
    fn node_key(&self, node: u32) -> Option<String> {
        let slice = ffi::engine_node_key(self.as_const(), node);
        unsafe { slice_to_string(slice) }
    }

    pub fn node_detail(&self, node: u32) -> NodeDetailDto {
        let p = self.as_const();
        let kind = ffi::engine_node_kind(p, node);
        let is_container = kind == KIND_ARRAY || kind == KIND_OBJECT;
        let array_index = if ffi::engine_node_is_array_element(p, node) != 0 {
            Some(ffi::engine_node_array_index(p, node))
        } else {
            None
        };
        NodeDetailDto {
            id: node,
            kind,
            child_count: ffi::engine_node_child_count(p, node),
            is_container,
            byte_offset: ffi::engine_node_byte_offset(p, node),
            byte_length: ffi::engine_node_byte_length(p, node),
            path: self.node_path(node),
            value: if is_container { None } else { self.node_value(node) },
            key: self.node_key(node),
            array_index,
        }
    }

    /// Parent chain from root to `node`, used for the breadcrumb.
    pub fn ancestors(&self, node: u32) -> Vec<AncestorDto> {
        let p = self.as_const();
        let mut ids = vec![node];
        let mut cur = node;
        loop {
            let parent = ffi::engine_node_parent(p, cur);
            if parent == NULL_NODE {
                break;
            }
            ids.push(parent);
            cur = parent;
        }
        ids.reverse();
        ids.into_iter()
            .map(|id| AncestorDto { id, label: self.ancestor_label(id) })
            .collect()
    }

    fn ancestor_label(&self, id: u32) -> String {
        let p = self.as_const();
        if ffi::engine_node_parent(p, id) == NULL_NODE {
            return "$".to_string();
        }
        if let Some(k) = self.node_key(id) {
            if !k.is_empty() {
                return k;
            }
        }
        if ffi::engine_node_is_array_element(p, id) != 0 {
            return format!("[{}]", ffi::engine_node_array_index(p, id));
        }
        "?".to_string()
    }

    pub fn run_query(&self, query: &str, limit: u32) -> Result<QueryRunDto, String> {
        let dto = self.run_query_once(query, limit)?;
        // A `lookup(...)` with no registered foreign-key index is an
        // implementation detail of evaluating the query, not a user
        // decision: build the index inline and re-run once. The single retry guards
        // against a re-reported hint looping forever.
        if let Some((source, key)) = &dto.missing_index {
            if self.create_index(source, key) {
                return self.run_query_once(query, limit);
            }
        }
        Ok(dto)
    }

    fn run_query_once(&self, query: &str, limit: u32) -> Result<QueryRunDto, String> {
        let cq = CString::new(query).map_err(|_| "query contains NUL byte".to_string())?;
        let handle = ffi::engine_query_run(self.as_const(), cq.as_ptr(), limit);
        if handle.is_null() {
            return Err(last_query_error());
        }
        let dto = unsafe { self.collect_results(handle) };
        ffi::engine_query_results_free(handle);
        Ok(dto)
    }

    /// Builds and registers a foreign-key index for the canonical
    /// `(source, key)` expressions the engine reported as missing.
    /// Returns true if the build succeeded.
    fn create_index(&self, source: &str, key: &str) -> bool {
        let (Ok(src), Ok(key)) = (CString::new(source), CString::new(key)) else {
            return false;
        };
        let stats = ffi::engine_query_create_index(self.as_const(), src.as_ptr(), key.as_ptr());
        stats.ok != 0
    }

    pub fn text_search(&self, needle: &str, limit: u32) -> Result<QueryRunDto, String> {
        let cq = CString::new(needle).map_err(|_| "needle contains NUL byte".to_string())?;
        let handle = ffi::engine_query_text_search(self.as_const(), cq.as_ptr(), limit);
        if handle.is_null() {
            return Ok(QueryRunDto::default());
        }
        let dto = unsafe { self.collect_results(handle) };
        ffi::engine_query_results_free(handle);
        Ok(dto)
    }

    unsafe fn collect_results(&self, handle: *mut QueryResults) -> QueryRunDto {
        let count = ffi::engine_query_results_count(handle);
        let mut rows = Vec::with_capacity(count as usize);
        for i in 0..count {
            let v: EngineQueryResultView = ffi::engine_query_results_at(handle, i);
            let node_id = if v.node_id == NULL_NODE { None } else { Some(v.node_id) };
            let child_count = match node_id {
                Some(id) if v.kind == KIND_ARRAY || v.kind == KIND_OBJECT => {
                    ffi::engine_node_child_count(self.as_const(), id)
                }
                _ => 0,
            };
            rows.push(RowDto {
                node_id,
                kind: v.kind,
                path: slice_to_string(v.path).unwrap_or_default(),
                preview: slice_to_string(v.preview).unwrap_or_default(),
                full_text: slice_to_string(v.full_text).unwrap_or_default(),
                child_count,
            });
        }
        let table = self.build_table(&rows);
        let missing_index = {
            let src = ffi::engine_query_results_missing_index_source(handle);
            let key = ffi::engine_query_results_missing_index_key(handle);
            if src.is_null() || key.is_null() {
                None
            } else {
                let src = cstr_ptr_to_string(src);
                let key = cstr_ptr_to_string(key);
                Some((src, key))
            }
        };
        QueryRunDto {
            rows,
            hit_limit: ffi::engine_query_results_hit_limit(handle) != 0,
            scanned_rows: ffi::engine_query_results_scanned_rows(handle),
            scanned_bytes: ffi::engine_query_results_scanned_bytes(handle),
            lookup_calls: ffi::engine_query_results_lookup_calls(handle),
            missing_index,
            table,
        }
    }

    /// Builds the spreadsheet projection: extracts each row's top-level
    /// fields, accumulates per-column frequency, orders columns by
    /// frequency (first-seen tiebreak), and decides if the result set
    /// is legitimately tabular.
    fn build_table(&self, rows: &[RowDto]) -> TableSnapshotDto {
        if rows.is_empty() {
            return TableSnapshotDto::default();
        }
        let mut out_rows: Vec<TableRowDto> = Vec::with_capacity(rows.len());
        let mut frequency: HashMap<String, usize> = HashMap::new();
        let mut order: HashMap<String, usize> = HashMap::new();
        let mut order_counter = 0usize;
        let mut fielded = 0usize;

        for (i, r) in rows.iter().enumerate() {
            let cells = self.extract_cells(r);
            if !cells.is_empty() {
                fielded += 1;
                for (k, _) in &cells {
                    *frequency.entry(k.clone()).or_insert(0) += 1;
                    if !order.contains_key(k) {
                        order.insert(k.clone(), order_counter);
                        order_counter += 1;
                    }
                }
            }
            let label = if !r.path.is_empty() && !r.path.starts_with("(synthetic)") {
                r.path.clone()
            } else {
                format!("[{i}]")
            };
            out_rows.push(TableRowDto {
                node_id: r.node_id,
                label,
                cells: cells.into_iter().collect(),
            });
        }

        let mut cols: Vec<(String, usize)> = frequency.into_iter().collect();
        cols.sort_by(|a, b| {
            if a.1 != b.1 {
                return b.1.cmp(&a.1);
            }
            order[&a.0].cmp(&order[&b.0])
        });
        cols.truncate(MAX_COLUMNS);
        let top_freq = cols.first().map(|(_, c)| *c).unwrap_or(0);
        let columns: Vec<String> = cols.into_iter().map(|(k, _)| k).collect();

        let is_tabular = rows.len() >= 2
            && fielded >= std::cmp::max(2, rows.len() * 4 / 10)
            && top_freq >= std::cmp::max(2, fielded * 6 / 10);

        TableSnapshotDto { columns, rows: out_rows, is_tabular }
    }

    /// Pulls one row's top-level fields into `(column, cell)` pairs.
    /// Document-backed object rows go through one children batch;
    /// synthetic rows lift `full_text` via the order-preserving parser.
    /// Non-object rows contribute no columns.
    fn extract_cells(&self, r: &RowDto) -> Vec<(String, TableCellDto)> {
        if let Some(nid) = r.node_id {
            if r.kind != KIND_OBJECT {
                return Vec::new();
            }
            let count = ffi::engine_node_child_count(self.as_const(), nid);
            let limit = count.min((MAX_COLUMNS as u32) * 2);
            return self
                .children(nid, 0, limit)
                .into_iter()
                .filter_map(|c| {
                    let key = c.key?;
                    let cell = if c.is_container {
                        TableCellDto {
                            kind: c.kind,
                            is_container: true,
                            text: None,
                            count: Some(c.child_count),
                        }
                    } else {
                        TableCellDto {
                            kind: c.kind,
                            is_container: false,
                            text: Some(c.preview),
                            count: None,
                        }
                    };
                    Some((key, cell))
                })
                .collect();
        }
        if !r.full_text.is_empty() {
            if let Some(entries) = parse_top_level_object(&r.full_text) {
                return entries;
            }
        }
        Vec::new()
    }

    /// Bitmask of node kinds produced by sampling the query's outputs
    /// (bit i = kind i). Drives the autocomplete key/array-accessor split.
    pub fn query_kinds(&self, query: &str, limit: u32) -> u8 {
        let Ok(cq) = CString::new(query) else { return 0 };
        ffi::engine_kinds_for_query(self.as_const(), cq.as_ptr(), limit)
    }

    /// Union of object keys produced by sampling the query's outputs.
    pub fn query_keys(&self, query: &str, limit: u32) -> Vec<String> {
        let Ok(cq) = CString::new(query) else { return Vec::new() };
        let owned = ffi::engine_keys_for_query(self.as_const(), cq.as_ptr(), limit);
        match unsafe { owned_to_string(owned) } {
            Some(json) => serde_json::from_str(&json).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    pub fn render(&self, query: &str, limit: u32, format: u8) -> Result<String, String> {
        let cq = CString::new(query).map_err(|_| "query contains NUL byte".to_string())?;
        let owned =
            ffi::engine_query_run_and_render(self.as_const(), cq.as_ptr(), limit, format);
        match unsafe { owned_to_string(owned) } {
            Some(s) => Ok(s),
            None => Err(last_query_error()),
        }
    }
}

// --- free functions over engine-global / parser state ------------------------

pub fn grammar_manifest() -> String {
    let owned = ffi::engine_grammar_manifest();
    unsafe { owned_to_string(owned) }.unwrap_or_else(|| "{}".to_string())
}

pub fn tokenize(source: &str) -> String {
    let Ok(cs) = CString::new(source) else {
        return "[]".to_string();
    };
    let owned = ffi::engine_tokenize(cs.as_ptr());
    unsafe { owned_to_string(owned) }.unwrap_or_else(|| "[]".to_string())
}

pub fn completion_context(source: &str, cursor_utf16: u32) -> Option<String> {
    let cs = CString::new(source).ok()?;
    let owned = ffi::engine_completion_context(cs.as_ptr(), cursor_utf16);
    unsafe { owned_to_string(owned) }
}

pub fn format_query(query: &str) -> Result<String, String> {
    let cs = CString::new(query).map_err(|_| "query contains NUL byte".to_string())?;
    let owned = ffi::engine_format_query(cs.as_ptr());
    match unsafe { owned_to_string(owned) } {
        Some(s) => Ok(s),
        None => Err(last_query_error()),
    }
}

// --- synthetic-row JSON projection -------------------------------------------
//
// A bounded, order-preserving parser used to lift a synthetic row's
// `full_text` (an aggregate/bucket output we never indexed) into its
// top-level fields for the table view. Only the top level is classified;
// nested containers are skipped while counting their immediate children.

const KIND_NULL: u8 = 0;
const KIND_BOOL: u8 = 1;
const KIND_NUMBER: u8 = 2;
const KIND_STRING: u8 = 3;

fn parse_top_level_object(s: &str) -> Option<Vec<(String, TableCellDto)>> {
    let b = s.as_bytes();
    let mut p = 0usize;
    skip_ws(b, &mut p);
    if p >= b.len() || b[p] != b'{' {
        return None;
    }
    p += 1;
    let mut out: Vec<(String, TableCellDto)> = Vec::new();
    skip_ws(b, &mut p);
    if p < b.len() && b[p] == b'}' {
        return Some(out);
    }
    loop {
        skip_ws(b, &mut p);
        let key = parse_string_decoded(b, &mut p)?;
        skip_ws(b, &mut p);
        if p >= b.len() || b[p] != b':' {
            return None;
        }
        p += 1;
        let cell = parse_value_cell(b, &mut p)?;
        out.push((key, cell));
        skip_ws(b, &mut p);
        if p < b.len() && b[p] == b',' {
            p += 1;
            continue;
        }
        if p < b.len() && b[p] == b'}' {
            return Some(out);
        }
        return None;
    }
}

fn parse_value_cell(b: &[u8], p: &mut usize) -> Option<TableCellDto> {
    skip_ws(b, p);
    if *p >= b.len() {
        return None;
    }
    match b[*p] {
        b'{' => {
            let count = parse_object_count(b, p)?;
            Some(TableCellDto { kind: 5, is_container: true, text: None, count: Some(count) })
        }
        b'[' => {
            let count = parse_array_count(b, p)?;
            Some(TableCellDto { kind: 4, is_container: true, text: None, count: Some(count) })
        }
        b'"' => {
            let start = *p;
            skip_string(b, p)?;
            let text = std::str::from_utf8(&b[start..*p]).ok()?.to_string();
            Some(TableCellDto {
                kind: KIND_STRING,
                is_container: false,
                text: Some(text),
                count: None,
            })
        }
        b't' | b'f' => {
            let text = read_literal(b, p);
            Some(TableCellDto { kind: KIND_BOOL, is_container: false, text: Some(text), count: None })
        }
        b'n' => {
            let text = read_literal(b, p);
            Some(TableCellDto { kind: KIND_NULL, is_container: false, text: Some(text), count: None })
        }
        b'-' | b'0'..=b'9' => {
            let text = read_number(b, p);
            Some(TableCellDto {
                kind: KIND_NUMBER,
                is_container: false,
                text: Some(text),
                count: None,
            })
        }
        _ => None,
    }
}

fn skip_ws(b: &[u8], p: &mut usize) {
    while *p < b.len() && matches!(b[*p], b' ' | b'\t' | b'\n' | b'\r') {
        *p += 1;
    }
}

fn skip_string(b: &[u8], p: &mut usize) -> Option<()> {
    if *p >= b.len() || b[*p] != b'"' {
        return None;
    }
    *p += 1;
    while *p < b.len() {
        match b[*p] {
            b'"' => {
                *p += 1;
                return Some(());
            }
            b'\\' => *p += 2,
            _ => *p += 1,
        }
    }
    None
}

fn parse_string_decoded(b: &[u8], p: &mut usize) -> Option<String> {
    if *p >= b.len() || b[*p] != b'"' {
        return None;
    }
    *p += 1;
    let mut out: Vec<u8> = Vec::new();
    while *p < b.len() {
        match b[*p] {
            b'"' => {
                *p += 1;
                return String::from_utf8(out).ok();
            }
            b'\\' => {
                *p += 1;
                if *p >= b.len() {
                    return None;
                }
                let e = b[*p];
                *p += 1;
                match e {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        if *p + 4 > b.len() {
                            return None;
                        }
                        let mut code: u32 = 0;
                        for _ in 0..4 {
                            code = code * 16 + hex_val(b[*p])?;
                            *p += 1;
                        }
                        let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    other => out.push(other),
                }
            }
            other => {
                out.push(other);
                *p += 1;
            }
        }
    }
    None
}

fn hex_val(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some((c - b'0') as u32),
        b'a'..=b'f' => Some((c - b'a' + 10) as u32),
        b'A'..=b'F' => Some((c - b'A' + 10) as u32),
        _ => None,
    }
}

fn read_literal(b: &[u8], p: &mut usize) -> String {
    let start = *p;
    while *p < b.len() && b[*p].is_ascii_alphabetic() {
        *p += 1;
    }
    String::from_utf8_lossy(&b[start..*p]).into_owned()
}

fn read_number(b: &[u8], p: &mut usize) -> String {
    let start = *p;
    while *p < b.len()
        && matches!(b[*p], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
    {
        *p += 1;
    }
    String::from_utf8_lossy(&b[start..*p]).into_owned()
}

fn skip_value(b: &[u8], p: &mut usize) -> Option<()> {
    skip_ws(b, p);
    if *p >= b.len() {
        return None;
    }
    match b[*p] {
        b'{' => {
            parse_object_count(b, p)?;
            Some(())
        }
        b'[' => {
            parse_array_count(b, p)?;
            Some(())
        }
        b'"' => skip_string(b, p),
        _ => {
            while *p < b.len()
                && !matches!(b[*p], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                *p += 1;
            }
            Some(())
        }
    }
}

fn parse_object_count(b: &[u8], p: &mut usize) -> Option<u32> {
    *p += 1; // consume '{'
    skip_ws(b, p);
    if *p < b.len() && b[*p] == b'}' {
        *p += 1;
        return Some(0);
    }
    let mut n = 0u32;
    loop {
        skip_ws(b, p);
        skip_string(b, p)?;
        skip_ws(b, p);
        if *p >= b.len() || b[*p] != b':' {
            return None;
        }
        *p += 1;
        skip_value(b, p)?;
        n += 1;
        skip_ws(b, p);
        if *p < b.len() && b[*p] == b',' {
            *p += 1;
            continue;
        }
        if *p < b.len() && b[*p] == b'}' {
            *p += 1;
            return Some(n);
        }
        return None;
    }
}

fn parse_array_count(b: &[u8], p: &mut usize) -> Option<u32> {
    *p += 1; // consume '['
    skip_ws(b, p);
    if *p < b.len() && b[*p] == b']' {
        *p += 1;
        return Some(0);
    }
    let mut n = 0u32;
    loop {
        skip_value(b, p)?;
        n += 1;
        skip_ws(b, p);
        if *p < b.len() && b[*p] == b',' {
            *p += 1;
            continue;
        }
        if *p < b.len() && b[*p] == b']' {
            *p += 1;
            return Some(n);
        }
        return None;
    }
}

// --- low-level conversions ---------------------------------------------------

fn zeroed_meta() -> EngineChildMeta {
    EngineChildMeta {
        id: 0,
        kind: 0,
        flags: 0,
        _pad: 0,
        child_count: 0,
        key_offset: 0,
        key_length: 0,
        array_index: 0,
        value_offset: 0,
        value_length: 0,
    }
}

fn path_to_cstring(p: &Path) -> Result<CString, String> {
    let s = p.to_str().ok_or_else(|| "non-UTF-8 path".to_string())?;
    CString::new(s).map_err(|_| "path contains NUL byte".to_string())
}

unsafe fn slice_to_string(s: EngineSlice) -> Option<String> {
    if s.data.is_null() || s.length == 0 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(s.data, s.length as usize);
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Reads an owned buffer into a `String`, then frees it through the
/// engine allocator. Returns `None` for the `{null, 0}` sentinel.
unsafe fn owned_to_string(b: EngineOwnedBytes) -> Option<String> {
    if b.data.is_null() || b.length == 0 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(b.data, b.length as usize);
    let out = String::from_utf8_lossy(bytes).into_owned();
    ffi::engine_free_owned_bytes(b);
    Some(out)
}

unsafe fn cstr_ptr_to_string(p: *const std::os::raw::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

fn last_error() -> String {
    let p = ffi::engine_last_error();
    unsafe { cstr_ptr_to_string(p) }
}

fn last_query_error() -> String {
    let p = ffi::engine_query_last_parse_error();
    let msg = unsafe { cstr_ptr_to_string(p) };
    if msg.is_empty() {
        "query failed".to_string()
    } else {
        let pos = ffi::engine_query_last_parse_error_position();
        format!("parse error at position {}: {}", pos, msg)
    }
}
