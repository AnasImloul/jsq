//! Per-node accessors over record-bearing nodes (containers and fat
//! strings). Primitives without records are surfaced through
//! [`super::children`] instead — those calls accept `(parent, source
//! offset/length)` rather than a node id.

use crate::document::{Document, FLAG_ARRAY_ELEMENT, NULL_NODE};

use super::{EngineOwnedBytes, EngineSlice};

#[no_mangle]
pub extern "C" fn engine_node_kind(doc: *const Document, node: u32) -> u8 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    d.record(node).map(|r| r.kind).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn engine_node_parent(doc: *const Document, node: u32) -> u32 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return NULL_NODE };
    d.record(node).map(|r| r.parent).unwrap_or(NULL_NODE)
}

#[no_mangle]
pub extern "C" fn engine_node_child_count(doc: *const Document, node: u32) -> u32 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    d.record(node).map(|r| r.child_count).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn engine_node_byte_offset(doc: *const Document, node: u32) -> u64 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    d.record(node).map(|r| r.offset).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn engine_node_byte_length(doc: *const Document, node: u32) -> u64 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    d.record(node).map(|r| r.length).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn engine_node_value_bytes(doc: *const Document, node: u32) -> EngineSlice {
    let Some(d) = (unsafe { doc.as_ref() }) else { return EngineSlice::empty() };
    match d.value_bytes(node) {
        Some(b) => EngineSlice::from_slice(b),
        None => EngineSlice::empty(),
    }
}

/// Returns a borrowed slice into either the source mmap (`source_flag != 0`)
/// or the document's decoded keys arena (`source_flag == 0`). Reads a
/// primitive child's key/value bytes via the offsets carried in
/// `EngineChildMeta`. The returned pointer is valid for the document's
/// lifetime.
#[no_mangle]
pub extern "C" fn engine_node_value_bytes_at(
    doc: *const Document,
    offset: u64,
    length: u64,
    source_flag: u8,
) -> EngineSlice {
    let Some(d) = (unsafe { doc.as_ref() }) else { return EngineSlice::empty() };
    let buf: &[u8] = if source_flag != 0 { &d.source_mmap[..] } else { d.keys() };
    let start = offset as usize;
    let end = match start.checked_add(length as usize) {
        Some(e) => e,
        None => return EngineSlice::empty(),
    };
    if end > buf.len() { return EngineSlice::empty(); }
    EngineSlice::from_slice(&buf[start..end])
}

#[no_mangle]
pub extern "C" fn engine_node_key(doc: *const Document, node: u32) -> EngineSlice {
    let Some(d) = (unsafe { doc.as_ref() }) else { return EngineSlice::empty() };
    match d.key_bytes(node) {
        Some(b) => EngineSlice::from_slice(b),
        None => EngineSlice::empty(),
    }
}

#[no_mangle]
pub extern "C" fn engine_node_array_index(doc: *const Document, node: u32) -> u32 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    match d.record(node) {
        // `key_or_index` is u64 (it doubles as a keys-arena byte offset
        // for object members); record-bearing array elements never
        // exceed u32::MAX since they're bounded by the record-id space,
        // so the cast is safe — but saturate defensively.
        Some(r) if r.flags & FLAG_ARRAY_ELEMENT != 0 => {
            u32::try_from(r.key_or_index).unwrap_or(u32::MAX)
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn engine_node_is_array_element(doc: *const Document, node: u32) -> u8 {
    let Some(d) = (unsafe { doc.as_ref() }) else { return 0 };
    match d.record(node) {
        Some(r) if r.flags & FLAG_ARRAY_ELEMENT != 0 => 1,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn engine_node_path(doc: *const Document, node: u32) -> EngineOwnedBytes {
    let Some(d) = (unsafe { doc.as_ref() }) else {
        return EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    };
    let bytes = crate::path::compute_path(d, node);
    owned_from_vec(bytes)
}

/// Decodes a JSON-string byte span (the bytes *between* the surrounding
/// quotes — NOT including them) into UTF-8. Mirrors the decoder used
/// internally for keys and values, so callers reading raw key/value
/// bytes off `EngineChildMeta` don't need their own implementation.
/// `{NULL, 0}` on a malformed escape sequence or null input.
#[no_mangle]
pub extern "C" fn engine_decode_json_string(
    data: *const u8,
    length: u64,
) -> EngineOwnedBytes {
    if data.is_null() {
        return EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 };
    }
    let inner = unsafe { std::slice::from_raw_parts(data, length as usize) };
    match crate::source_scan::decode_json_string_inner(inner) {
        Some(bytes) => owned_from_vec(bytes),
        None => EngineOwnedBytes { data: std::ptr::null_mut(), length: 0 },
    }
}

fn owned_from_vec(bytes: Vec<u8>) -> EngineOwnedBytes {
    let length = bytes.len() as u64;
    let mut boxed = bytes.into_boxed_slice();
    let ptr = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    EngineOwnedBytes { data: ptr, length }
}
