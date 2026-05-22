//! One-level source-byte scanners.
//!
//! Under the hybrid emit-gate, primitives don't have records — they live
//! only in the source bytes. These helpers walk a container's source
//! span one level deep, interleaving skippable record IDs (containers
//! and fat-strings) with inline `Value` scalars synthesized from primitive
//! bytes. The skippable cursor walks `subtree_size`-deltas in lockstep
//! with the source position; whenever the source pointer reaches the
//! next skippable's offset, we take its record id; otherwise we parse
//! the value inline.

use crate::container_scan::ContainerOpen;
use crate::document::{Document, NodeKind};
use crate::source_scan::{
    parse_string_decoded, parse_string_view, skip_inline_value, skip_number, skip_string, skip_ws,
};

use super::super::value::Value;

/// Iterates one level deep through a container's children. Emits each
/// child as either `Value::Node(id)` (skippable) or a primitive
/// `Value::Null/Bool/Number/Str` (inline). Returns false if the sink
/// requested early termination.
pub(super) fn scan_iterate(
    doc: &Document,
    parent_id: u32,
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let Some(open) = ContainerOpen::new(doc, parent_id) else { return true };
    open.issue_willneed_hint(doc);
    let mut next_skippable = open.initial_next_skippable;
    let close = open.close_byte();
    let source = open.source;
    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == close { return true; }

    loop {
        if open.kind == NodeKind::Object {
            if !skip_string(source, &mut pos) { return true; }
            skip_ws(source, &mut pos);
            if pos >= source.len() || source[pos] != b':' { return true; }
            pos += 1;
            skip_ws(source, &mut pos);
        }

        if open.at_skippable(doc, pos, next_skippable) {
            let r = doc.record(next_skippable).unwrap();
            let id = next_skippable;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
            if !sink(Value::Node(id)) { return false; }
            pos += r.length as usize;
        } else {
            // Inline scalar — no record, so the `Tap` byte-counter
            // can't attribute its bytes. Count them here, against the
            // raw source span we just consumed.
            let value_start = pos;
            let v = parse_inline_value(source, &mut pos);
            super::bump_scanned_bytes((pos - value_start) as u64);
            if !sink(v) { return false; }
        }

        skip_ws(source, &mut pos);
        if pos >= source.len() { break; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); continue; }
            c if c == close => break,
            _ => break,
        }
    }
    true
}

/// Iterates a container's children and, for each child object,
/// extracts and emits the value of member `field_name`. Children that
/// aren't objects, and objects that don't have the named member, are
/// silently skipped — matching `Pipe(Iterate, Field(name))` semantics.
///
/// Fused into one scanner so the per-row work avoids: the `walk`
/// dispatch for `Field`, the `doc.record(child_id)` re-lookup inside
/// `scan_object_field`, and the closure indirection at the pipe seam.
pub(super) fn scan_iterate_field(
    doc: &Document,
    parent_id: u32,
    field_name: &[u8],
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let Some(open) = ContainerOpen::new(doc, parent_id) else { return true };
    open.issue_willneed_hint(doc);
    let mut next_skippable = open.initial_next_skippable;
    let close = open.close_byte();
    let source = open.source;
    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == close { return true; }

    // Slot index where `field_name` was last located inside a child
    // object. Cube/event-shaped data has stable key order across
    // siblings, so a hit at slot N typically holds for every subsequent
    // child — the hint lets `scan_object_field_with_hint` skip the
    // byte-comparison on pre-target keys.
    let mut hint_slot: Option<usize> = None;

    loop {
        if open.kind == NodeKind::Object {
            if !skip_string(source, &mut pos) { return true; }
            skip_ws(source, &mut pos);
            if pos >= source.len() || source[pos] != b':' { return true; }
            pos += 1;
            skip_ws(source, &mut pos);
        }

        if open.at_skippable(doc, pos, next_skippable) {
            let child_id = next_skippable;
            let child_record = *doc.record(child_id).unwrap();
            let child_length = child_record.length as usize;
            next_skippable = open.advance_skippable(next_skippable, child_record.subtree_size);

            // We had to read this whole child to look for the field —
            // that's the actual source bytes touched. `Tap` can't see
            // this work because the emitted value (when found) is a
            // sub-tree, not the whole child. Count it here so the
            // "bytes scanned" stat reflects what the engine really
            // walked rather than only what survived to the sink.
            super::bump_scanned_bytes(child_length as u64);

            if NodeKind::from_u8(child_record.kind) == NodeKind::Object {
                if let Some(v) = scan_object_field_with_hint(
                    doc,
                    child_id,
                    child_record,
                    field_name,
                    &mut hint_slot,
                ) {
                    if !sink(v) { return false; }
                }
            }
            pos += child_length;
        } else {
            // Inline child (small primitive directly in the array).
            // Not an object so the field-extraction is a no-op, but we
            // still scanned its bytes to confirm.
            let inline_start = pos;
            skip_inline_value(source, &mut pos);
            super::bump_scanned_bytes((pos - inline_start) as u64);
        }

        skip_ws(source, &mut pos);
        if pos >= source.len() { break; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); continue; }
            c if c == close => break,
            _ => break,
        }
    }
    true
}

/// `scan_object_field` variant that takes the child's record by value
/// (saves the per-call `doc.record(parent_id)` lookup) and consults a
/// caller-provided `hint_slot`. The hint short-circuits the byte
/// comparison on each pre-target key — we still parse the key
/// boundary and skip past its value, but don't pay the `name == key`
/// check until we reach the predicted slot. On miss (predicted slot's
/// key isn't `name`), falls back to comparing every subsequent key.
fn scan_object_field_with_hint(
    doc: &Document,
    parent_id: u32,
    parent: crate::document::NodeRecord,
    name: &[u8],
    hint_slot: &mut Option<usize>,
) -> Option<Value> {
    let open = ContainerOpen::from_record(doc, parent_id, parent)?;
    if open.kind != NodeKind::Object { return None; }
    let mut next_skippable = open.initial_next_skippable;
    let source = open.source;

    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == b'}' { return None; }

    let predicted = *hint_slot;
    let mut slot: usize = 0;

    loop {
        let key_start_pos = pos;
        let key_match: bool = if predicted == Some(slot) || predicted.is_none() {
            let key = parse_string_view(source, &mut pos)?;
            key.as_ref() == name
        } else {
            if !skip_string(source, &mut pos) { return None; }
            false
        };
        skip_ws(source, &mut pos);
        if pos >= source.len() || source[pos] != b':' { return None; }
        pos += 1;
        skip_ws(source, &mut pos);

        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        if key_match {
            *hint_slot = Some(slot);
            return Some(if is_skippable {
                Value::Node(next_skippable)
            } else {
                parse_inline_value(source, &mut pos)
            });
        }

        // Predicted slot's key didn't match — drop the hint and
        // restart comparing every remaining key from the same position.
        if predicted == Some(slot) {
            pos = key_start_pos;
            *hint_slot = None;
            return scan_object_field_linear_after_hint_miss(
                doc, &open, name, pos, next_skippable, slot, hint_slot,
            );
        }

        if is_skippable {
            let r = doc.record(next_skippable).unwrap();
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }
        skip_ws(source, &mut pos);
        if pos >= source.len() { return None; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); slot += 1; continue; }
            b'}' => return None,
            _ => return None,
        }
    }
}

/// Continuation of `scan_object_field_with_hint` taken when the slot
/// hint pointed at a non-matching key. Resumes the linear scan with
/// byte-comparison enabled on every remaining key, and updates the
/// hint on a successful match so subsequent calls re-acquire a
/// useful prediction.
fn scan_object_field_linear_after_hint_miss(
    doc: &Document,
    open: &ContainerOpen<'_>,
    name: &[u8],
    mut pos: usize,
    mut next_skippable: u32,
    mut slot: usize,
    hint_slot: &mut Option<usize>,
) -> Option<Value> {
    let source = open.source;

    loop {
        let key = parse_string_view(source, &mut pos)?;
        let matched = key.as_ref() == name;
        skip_ws(source, &mut pos);
        if pos >= source.len() || source[pos] != b':' { return None; }
        pos += 1;
        skip_ws(source, &mut pos);

        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        if matched {
            *hint_slot = Some(slot);
            return Some(if is_skippable {
                Value::Node(next_skippable)
            } else {
                parse_inline_value(source, &mut pos)
            });
        }

        if is_skippable {
            let r = doc.record(next_skippable).unwrap();
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }
        skip_ws(source, &mut pos);
        if pos >= source.len() { return None; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); slot += 1; continue; }
            b'}' => return None,
            _ => return None,
        }
    }
}

/// Looks up a single field by name in an object. Returns the matching
/// value (`Value::Node` for skippable, primitive otherwise), or `None`
/// if the key isn't present.
pub(super) fn scan_object_field(doc: &Document, parent_id: u32, name: &[u8]) -> Option<Value> {
    let open = ContainerOpen::new(doc, parent_id)?;
    if open.kind != NodeKind::Object { return None; }
    let mut next_skippable = open.initial_next_skippable;
    let source = open.source;

    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == b'}' { return None; }

    loop {
        // Borrow-or-decode key view; on the all-ASCII no-escape
        // common path this is a `Cow::Borrowed` — zero allocations,
        // direct slice equality against `name`.
        let key = parse_string_view(source, &mut pos)?;
        skip_ws(source, &mut pos);
        if pos >= source.len() || source[pos] != b':' { return None; }
        pos += 1;
        skip_ws(source, &mut pos);

        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        if key.as_ref() == name {
            return Some(if is_skippable {
                Value::Node(next_skippable)
            } else {
                parse_inline_value(source, &mut pos)
            });
        }

        if is_skippable {
            let r = doc.record(next_skippable).unwrap();
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }
        skip_ws(source, &mut pos);
        if pos >= source.len() { return None; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); continue; }
            b'}' => return None,
            _ => return None,
        }
    }
}

/// Single-pass field-set equality. Walks the object's child chain
/// **once**, checking each key against `fields`; on a hit, validates
/// the member's value against `target` via the caller-supplied
/// `value_eq` predicate. Returns `true` iff every field in `fields`
/// was present and `value_eq` accepted its value; returns `false` on
/// any structural problem, any missing field, or the first value
/// mismatch — short-circuits as soon as it knows the answer can't
/// change.
pub(super) fn scan_field_set_equals(
    doc: &Document,
    parent_id: u32,
    fields: &[String],
    target: &Value,
    mut value_eq: impl FnMut(&Value, &Value) -> bool,
) -> bool {
    if fields.is_empty() {
        return true;
    }
    let Some(open) = ContainerOpen::new(doc, parent_id) else { return false };
    if open.kind != NodeKind::Object { return false; }
    let mut next_skippable = open.initial_next_skippable;
    let source = open.source;

    // Per-slot match flags. `fields.len()` is small (typically ≤ ~16),
    // so a linear scan over the slice on every key is cheap and beats
    // a hashmap. >64 falls back to the heap-backed variant.
    let mut matched: [bool; 64] = [false; 64];
    let n = fields.len();
    if n > matched.len() {
        return scan_field_set_equals_heap(doc, parent_id, fields, target, value_eq);
    }
    let mut remaining = n;

    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == b'}' {
        return remaining == 0;
    }

    loop {
        let Some(key) = parse_string_view(source, &mut pos) else { return false };
        skip_ws(source, &mut pos);
        if pos >= source.len() || source[pos] != b':' { return false; }
        pos += 1;
        skip_ws(source, &mut pos);

        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        // Find the (still-unmatched) field this key satisfies.
        let key_bytes: &[u8] = key.as_ref();
        let mut slot: Option<usize> = None;
        if remaining > 0 {
            for (i, f) in fields.iter().enumerate() {
                if !matched[i] && f.as_bytes() == key_bytes {
                    slot = Some(i);
                    break;
                }
            }
        }

        if let Some(i) = slot {
            let member = if is_skippable {
                Value::Node(next_skippable)
            } else {
                parse_inline_value(source, &mut pos)
            };
            if !value_eq(&member, target) {
                return false;
            }
            matched[i] = true;
            remaining -= 1;

            if is_skippable {
                let r = doc.record(next_skippable).unwrap();
                pos += r.length as usize;
                next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
            }

            if remaining == 0 {
                return true;
            }
        } else if is_skippable {
            let r = doc.record(next_skippable).unwrap();
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }

        skip_ws(source, &mut pos);
        if pos >= source.len() { return remaining == 0; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); continue; }
            b'}' => return remaining == 0,
            _ => return false,
        }
    }
}

/// Heap-backed fallback for field sets longer than the stack cap.
/// Same logic as `scan_field_set_equals`, with the match flags
/// promoted to a `Vec<bool>`.
fn scan_field_set_equals_heap(
    doc: &Document,
    parent_id: u32,
    fields: &[String],
    target: &Value,
    mut value_eq: impl FnMut(&Value, &Value) -> bool,
) -> bool {
    let Some(open) = ContainerOpen::new(doc, parent_id) else { return false };
    if open.kind != NodeKind::Object { return false; }
    let mut next_skippable = open.initial_next_skippable;
    let source = open.source;

    let mut matched = vec![false; fields.len()];
    let mut remaining = fields.len();
    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == b'}' { return remaining == 0; }
    loop {
        let Some(key) = parse_string_view(source, &mut pos) else { return false };
        skip_ws(source, &mut pos);
        if pos >= source.len() || source[pos] != b':' { return false; }
        pos += 1;
        skip_ws(source, &mut pos);
        let is_skippable = open.at_skippable(doc, pos, next_skippable);
        let key_bytes: &[u8] = key.as_ref();
        let mut slot: Option<usize> = None;
        if remaining > 0 {
            for (i, f) in fields.iter().enumerate() {
                if !matched[i] && f.as_bytes() == key_bytes {
                    slot = Some(i); break;
                }
            }
        }
        if let Some(i) = slot {
            let member = if is_skippable {
                Value::Node(next_skippable)
            } else {
                parse_inline_value(source, &mut pos)
            };
            if !value_eq(&member, target) { return false; }
            matched[i] = true;
            remaining -= 1;
            if is_skippable {
                let r = doc.record(next_skippable).unwrap();
                pos += r.length as usize;
                next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
            }
            if remaining == 0 { return true; }
        } else if is_skippable {
            let r = doc.record(next_skippable).unwrap();
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }
        skip_ws(source, &mut pos);
        if pos >= source.len() { return remaining == 0; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); continue; }
            b'}' => return remaining == 0,
            _ => return false,
        }
    }
}

/// Indexes into an array by 0-based slot. Returns the slot's value as
/// either `Value::Node` (skippable) or a primitive `Value`.
pub(super) fn scan_array_index(doc: &Document, parent_id: u32, slot: usize) -> Option<Value> {
    let open = ContainerOpen::new(doc, parent_id)?;
    if open.kind != NodeKind::Array { return None; }
    let mut next_skippable = open.initial_next_skippable;
    let source = open.source;

    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == b']' { return None; }

    let mut idx: usize = 0;
    loop {
        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        if idx == slot {
            return Some(if is_skippable {
                Value::Node(next_skippable)
            } else {
                parse_inline_value(source, &mut pos)
            });
        }
        if is_skippable {
            let r = doc.record(next_skippable).unwrap();
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }
        skip_ws(source, &mut pos);
        if pos >= source.len() { return None; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); idx += 1; continue; }
            b']' => return None,
            _ => return None,
        }
    }
}

/// Walks the subtree rooted at `id` in document order, emitting the
/// root and every descendant. Under the hybrid emit-gate, primitive
/// descendants don't have records, so we do two passes: a fast linear
/// scan of the records section emits all skippable descendants, then
/// a source-scan of each container surfaces its primitive members.
pub(super) fn descend_emit(doc: &Document, id: u32, sink: &mut dyn FnMut(Value) -> bool) -> bool {
    let records = doc.records();
    let r = &records[id as usize];
    let end = (id + r.subtree_size) as usize;

    // Phase 1: every record-bearing descendant (containers + fat strings),
    // including self, in pre-order via the contiguous subtree slice.
    for k in (id as usize)..end {
        if !sink(Value::Node(k as u32)) {
            return false;
        }
    }

    // Phase 2: every primitive descendant. We visit each container in
    // the subtree and source-scan it for primitive members; skippable
    // children were already emitted in phase 1, so we filter them out.
    for k in (id as usize)..end {
        let kind = NodeKind::from_u8(records[k].kind);
        if !matches!(kind, NodeKind::Object | NodeKind::Array) { continue; }
        let mut early_stop = false;
        let cont = scan_iterate(doc, k as u32, &mut |v| {
            if matches!(v, Value::Node(_)) {
                // Already emitted in phase 1.
                return true;
            }
            if !sink(v) {
                early_stop = true;
                return false;
            }
            true
        });
        if early_stop || !cont { return false; }
    }
    true
}

/// Subtree walk that emits *only* descendants whose object-member key
/// matches `name`. Saves the cost of materialising every intermediate
/// node when the user wrote `.X.*.Y` and only cares about `Y`s.
pub(super) fn descend_field_emit(
    doc: &Document,
    id: u32,
    name: &[u8],
    sink: &mut dyn FnMut(Value) -> bool,
) -> bool {
    let records = doc.records();
    let r = &records[id as usize];
    let end = (id + r.subtree_size) as usize;
    // Self is included in the walk — `.**.foo` over a subtree should
    // also match `foo` on the root, mirroring `Descend | Field` whose
    // `Descend` arm emits the input itself before its descendants.
    for k in (id as usize)..end {
        if NodeKind::from_u8(records[k].kind) != NodeKind::Object { continue; }
        if let Some(v) = scan_object_field(doc, k as u32, name) {
            if !sink(v) { return false; }
        }
    }
    true
}

/// Parses a JSON value at `*pos` (must already be at a non-whitespace
/// byte) and returns it as a `Value`. Advances `*pos` past the value.
fn parse_inline_value(src: &[u8], pos: &mut usize) -> Value {
    if *pos >= src.len() { return Value::Null; }
    match src[*pos] {
        b'"' => match parse_string_decoded(src, pos) {
            Some(bytes) => Value::Str(String::from_utf8_lossy(&bytes).into_owned()),
            None => Value::Null,
        },
        b't' => {
            if src.len() >= *pos + 4 && &src[*pos..*pos + 4] == b"true" {
                *pos += 4;
                Value::Bool(true)
            } else {
                *pos = src.len();
                Value::Null
            }
        }
        b'f' => {
            if src.len() >= *pos + 5 && &src[*pos..*pos + 5] == b"false" {
                *pos += 5;
                Value::Bool(false)
            } else {
                *pos = src.len();
                Value::Null
            }
        }
        b'n' => {
            if src.len() >= *pos + 4 && &src[*pos..*pos + 4] == b"null" {
                *pos += 4;
                Value::Null
            } else {
                *pos = src.len();
                Value::Null
            }
        }
        b'-' | b'0'..=b'9' => {
            let start = *pos;
            skip_number(src, pos);
            let s = std::str::from_utf8(&src[start..*pos]).unwrap_or("0");
            Value::Number(s.parse::<f64>().unwrap_or(0.0))
        }
        _ => Value::Null,
    }
}
