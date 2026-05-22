//! Child enumeration. Walks one level of a container's children,
//! interleaving record-bearing children (containers, fat strings) with
//! primitive children (which don't have records under the hybrid
//! emit-gate). Both kinds are surfaced as [`EngineChildMeta`] rows;
//! callers distinguish them by `id == NULL_NODE`.
//!
//! For pagination over very large containers, prefer
//! [`engine_node_children_meta_batch_resume`] over the offset-based
//! [`engine_node_children_meta_batch`] — the resume variant carries
//! state across calls so each batch is O(batch_size) rather than
//! O(offset + batch_size).

use crate::container_scan::ContainerOpen;
use crate::document::{
    Document, NodeKind, FLAG_ARRAY_ELEMENT, FLAG_KEY_IN_SOURCE, FLAG_OBJECT_MEMBER, NULL_NODE,
};
use crate::source_scan::{peek_kind, skip_inline_value, skip_string, skip_ws};

/// One row's worth of metadata for tree-view rendering. Under the
/// hybrid emit-gate, primitive children don't have records, so this
/// struct carries enough source-byte information for the UI to render
/// them inline without a node id.
///
/// Identification:
/// - `id != NULL_NODE` → record-bearing (container or fat string).
/// - `id == NULL_NODE` → primitive child; (`parent`, `array_index` or
///   `key_or_index_in_source`) identifies it.
///
/// Keys (object members only):
/// - `flags & FLAG_KEY_IN_SOURCE == 0` → `key_offset` indexes the
///   document's keys arena (decoded UTF-8).
/// - `flags & FLAG_KEY_IN_SOURCE != 0` → `key_offset` indexes the
///   source mmap (raw bytes, between the JSON string's quotes).
///
/// Values:
/// - `value_offset`/`value_length` always describe a slice of the
///   source mmap. For containers/fat-strings, these match
///   `engine_node_byte_offset` / `_length` for `id`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct EngineChildMeta {
    pub id: u32,
    pub kind: u8,
    pub flags: u8,
    pub _pad: u16,
    pub child_count: u32,
    /// Object members: byte offset (see flags for which buffer).
    /// Array elements / non-members: 0.
    pub key_offset: u64,
    pub key_length: u32,
    /// Array elements: 0-based index. Object members: 0.
    pub array_index: u32,
    /// Source byte offset of this child's value.
    pub value_offset: u64,
    /// Source byte length of this child's value. `u64` so multi-GB
    /// containers (e.g. the root of a 10GB JSON) don't truncate.
    pub value_length: u64,
}

/// Resumable scan state for paginating through a container's children
/// without re-scanning from byte 0 each call. The caller initialises
/// `pos = 0` and `next_skippable = ENGINE_NODE_NONE` (the FFI fills in
/// the real first-skippable id on the first call); subsequent calls
/// pass the same struct back in.
///
/// `pos` is a `u64` rather than `u32` — files in the multi-GB range
/// have byte positions that don't fit in 32 bits, and silent truncation
/// of `pos` at the 4 GB boundary corrupts the iterator state and made
/// the UI's "Loading more…" stub stick on huge containers.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct EngineScanState {
    /// Byte offset within the parent's source span. `u64::MAX` on the
    /// first call (the "uninitialised" sentinel — distinct from the
    /// legal cursor value 0 which points at the parent's opening
    /// bracket); updated by each call to one past the last emitted
    /// child.
    pub pos: u64,
    /// Record id of the next skippable child whose offset hasn't been
    /// reached yet, or `NULL_NODE` once the chain is exhausted. The
    /// caller sets this to `NULL_NODE` on the first call; the FFI
    /// detects that "uninitialised" sentinel and computes the real
    /// first skippable from the parent record.
    pub next_skippable: u32,
    /// Running array index for array parents; ignored for objects.
    pub array_index: u32,
}

/// Sentinel for "scan state hasn't entered the container yet". Picked
/// over `0` so it can't collide with the legal cursor value at the
/// parent's opening bracket — a previous design used `0` and made
/// extending the iterator with a "seek to byte X" entry point unsafe.
pub const ENGINE_SCAN_STATE_FRESH: u64 = u64::MAX;

/// Walks every child of `node` once and writes per-kind totals into the
/// six-slot output buffer (indexed by `NodeKind as u8`: 0=null, 1=bool,
/// 2=number, 3=string, 4=array, 5=object). Returns the total number of
/// children processed. Includes primitive children (which don't have
/// records under the hybrid emit-gate) by source-scanning the parent.
#[no_mangle]
pub extern "C" fn engine_node_children_kind_counts(
    doc: *const Document,
    node: u32,
    out: *mut u32, // length 6
) -> u32 {
    if out.is_null() { return 0; }
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    let counts = unsafe { std::slice::from_raw_parts_mut(out, 6) };
    counts.fill(0);

    let mut total: u32 = 0;
    scan_children(d, node, |kind, _| {
        let k = kind as u8 as usize;
        if k < 6 { counts[k] += 1; }
        total += 1;
        true
    });
    total
}

/// Fills `out` with up to `max` ChildMeta entries for `parent`'s
/// children starting at `offset`. Returns the number actually written.
///
/// **Note:** when paginating, prefer
/// [`engine_node_children_meta_batch_resume`] — it carries scan state
/// across calls so each batch is O(batch_size) instead of
/// O(offset + batch_size). The offset-based form here re-scans from
/// the first child every call; for huge containers (>>10K children)
/// that's quadratic over the full enumeration.
#[no_mangle]
pub extern "C" fn engine_node_children_meta_batch(
    doc: *const Document,
    parent: u32,
    offset: u32,
    max: u32,
    out: *mut EngineChildMeta,
) -> u32 {
    if out.is_null() || max == 0 { return 0; }
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };

    let mut skipped: u32 = 0;
    let mut written: u32 = 0;
    scan_children(d, parent, |_, meta| {
        if skipped < offset {
            skipped += 1;
            return true;
        }
        if written >= max { return false; }
        unsafe { std::ptr::write(out.add(written as usize), *meta); }
        written += 1;
        written < max
    });
    written
}

/// Stateful counterpart to [`engine_node_children_meta_batch`]. Reads
/// the next up-to-`max` children starting from where the previous call
/// left off, mutating `state` in place. Returns the number of entries
/// written (0 when the parent's children are exhausted).
///
/// First call: caller sets `state` to
/// `{ pos: ENGINE_SCAN_STATE_FRESH, next_skippable: NULL_NODE,
///    array_index: 0 }`. Subsequent calls pass the same struct back in
/// — its contents are opaque to the caller; the FFI updates them
/// between calls.
///
/// Total cost across all calls to enumerate a container with N children
/// is O(source_bytes_in_container), not O(N²) as the offset-based form.
#[no_mangle]
pub extern "C" fn engine_node_children_meta_batch_resume(
    doc: *const Document,
    parent: u32,
    state: *mut EngineScanState,
    max: u32,
    out: *mut EngineChildMeta,
) -> u32 {
    if out.is_null() || max == 0 || state.is_null() { return 0; }
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    let s = unsafe { &mut *state };

    let mut written: u32 = 0;
    let _ = scan_children_resume(d, parent, s, |_, meta| {
        if written >= max { return false; }
        unsafe { std::ptr::write(out.add(written as usize), *meta); }
        written += 1;
        written < max
    });
    written
}

/// Fills `out_ids` with up to `max` child IDs starting from the `offset`-th
/// child of `parent`. Returns the number actually written. Walking the
/// subtree-size-derived sibling chain inside Rust avoids the per-
/// iteration FFI overhead the caller would otherwise pay.
#[no_mangle]
pub extern "C" fn engine_node_children_batch(
    doc: *const Document,
    parent: u32,
    offset: u32,
    max: u32,
    out_ids: *mut u32,
) -> u32 {
    if out_ids.is_null() || max == 0 {
        return 0;
    }
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    let Some(rec) = d.record(parent) else { return 0 };

    // checked_add hardening: a corrupted subtree_size mustn't wrap
    // and alias an unrelated record id.
    let parent_end = parent
        .checked_add(rec.subtree_size)
        .unwrap_or(d.records().len() as u32);
    let mut cur = if rec.subtree_size > 1 {
        parent.checked_add(1).unwrap_or(NULL_NODE)
    } else {
        NULL_NODE
    };
    let mut skipped: u32 = 0;
    while cur != NULL_NODE && skipped < offset {
        let Some(r) = d.record(cur) else { return 0 };
        cur = match cur.checked_add(r.subtree_size) {
            Some(next) if next < parent_end => next,
            _ => NULL_NODE,
        };
        skipped += 1;
    }

    let mut written: u32 = 0;
    while cur != NULL_NODE && written < max {
        unsafe {
            std::ptr::write(out_ids.add(written as usize), cur);
        }
        let Some(r) = d.record(cur) else { break };
        cur = match cur.checked_add(r.subtree_size) {
            Some(next) if next < parent_end => next,
            _ => NULL_NODE,
        };
        written += 1;
    }
    written
}

/// Walks one level of `parent`'s children from the start, building an
/// `EngineChildMeta` for each and feeding it to `sink`. Convenience
/// wrapper over [`scan_children_resume`] with a fresh state.
fn scan_children<F: FnMut(NodeKind, &EngineChildMeta) -> bool>(
    doc: &Document,
    parent_id: u32,
    sink: F,
) {
    let mut state = EngineScanState {
        pos: ENGINE_SCAN_STATE_FRESH,
        next_skippable: NULL_NODE,
        array_index: 0,
    };
    scan_children_resume(doc, parent_id, &mut state, sink);
}

/// Resumable variant: walks from `state.pos` forward, mutating `state`
/// so subsequent calls continue where this one stopped. Returns `true`
/// when the iteration completed naturally; `false` when the sink
/// requested early termination (caller can resume by passing the same
/// state back in). On the first call, `state.next_skippable` should be
/// `NULL_NODE` (sentinel meaning "uninitialised") — the function then
/// computes the real first skippable from the parent record.
fn scan_children_resume<F: FnMut(NodeKind, &EngineChildMeta) -> bool>(
    doc: &Document,
    parent_id: u32,
    state: &mut EngineScanState,
    mut sink: F,
) -> bool {
    let Some(open) = ContainerOpen::new(doc, parent_id) else { return true };
    let source = open.source;
    let parent_offset = open.parent_offset;
    let close = open.close_byte();

    // First-call initialisation. The fresh sentinel is `u64::MAX` (not
    // 0) because 0 is a legal cursor — it points at the opening
    // bracket. Empty containers go straight to "exhausted".
    if state.pos == ENGINE_SCAN_STATE_FRESH {
        let mut pos: usize = 1;
        skip_ws(source, &mut pos);
        if pos < source.len() && source[pos] == close {
            state.pos = source.len() as u64;
            return true;
        }
        state.pos = pos as u64;
        state.next_skippable = open.initial_next_skippable;
        state.array_index = 0;
    }

    let mut pos = state.pos as usize;
    let mut next_skippable = state.next_skippable;
    let mut array_index = state.array_index;

    let mut completed = true;
    'outer: loop {
        if pos >= source.len() { break; }

        let mut raw_key_offset: u64 = 0;
        let mut raw_key_length: u32 = 0;
        if open.kind == NodeKind::Object {
            let key_start = pos;
            if !skip_string(source, &mut pos) { break; }
            raw_key_offset = (parent_offset + key_start + 1) as u64;
            raw_key_length = ((pos - 1) - (key_start + 1)) as u32;
            skip_ws(source, &mut pos);
            if pos >= source.len() || source[pos] != b':' { break; }
            pos += 1;
            skip_ws(source, &mut pos);
        }

        let value_pos = pos;
        let abs_value_pos = (parent_offset + value_pos) as u64;
        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        let meta = if is_skippable {
            let r = *doc.record(next_skippable).unwrap();
            let (key_offset, key_length) = if open.kind == NodeKind::Object {
                (r.key_or_index as u64, r.key_length as u32)
            } else {
                (0, 0)
            };
            let m = EngineChildMeta {
                id: next_skippable,
                kind: r.kind,
                flags: r.flags,
                _pad: 0,
                child_count: r.child_count,
                key_offset,
                key_length,
                array_index: if open.kind == NodeKind::Array { array_index } else { 0 },
                value_offset: r.offset,
                value_length: r.length,
            };
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
            m
        } else {
            let kind = peek_kind(source, pos);
            skip_inline_value(source, &mut pos);
            let value_length = (pos - value_pos) as u64;
            let flags = if open.kind == NodeKind::Object {
                FLAG_OBJECT_MEMBER | FLAG_KEY_IN_SOURCE
            } else {
                FLAG_ARRAY_ELEMENT
            };
            EngineChildMeta {
                id: NULL_NODE,
                kind,
                flags,
                _pad: 0,
                child_count: 0,
                key_offset: raw_key_offset,
                key_length: raw_key_length,
                array_index: if open.kind == NodeKind::Array { array_index } else { 0 },
                value_offset: abs_value_pos,
                value_length,
            }
        };

        // Persist *next* sibling's position before calling the sink so
        // that an early stop saves a state pointing at the next child,
        // not the one we just emitted.
        if open.kind == NodeKind::Array { array_index += 1; }
        let mut next_pos = pos;
        skip_ws(source, &mut next_pos);
        if next_pos < source.len() {
            match source[next_pos] {
                b',' => { next_pos += 1; skip_ws(source, &mut next_pos); }
                c if c == close => { next_pos = source.len(); }
                _ => { next_pos = source.len(); }
            }
        }

        if !sink(NodeKind::from_u8(meta.kind), &meta) {
            pos = next_pos;
            completed = false;
            break 'outer;
        }
        pos = next_pos;
    }

    state.pos = pos as u64;
    state.next_skippable = next_skippable;
    state.array_index = array_index;
    completed
}
