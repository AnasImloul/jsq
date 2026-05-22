//! Computes a jq-style path string for any node by walking the parent chain.
//!
//! Output rules:
//! - Root node: "."
//! - Object child with simple identifier key: ".foo"
//! - Object child with non-identifier key: `["weird key"]` (no leading dot)
//! - Array element: "[0]"
//!
//! When the first non-root segment is bracket-style, a leading "." is
//! prepended so the path always starts with a dot or bracket form rooted in
//! identity.

use crate::container_scan::ContainerOpen;
use crate::document::{Document, NodeKind, FLAG_ARRAY_ELEMENT, FLAG_OBJECT_MEMBER};
use crate::source_scan::{decode_json_string_inner, skip_inline_value, skip_string, skip_ws};

enum Segment<'a> {
    Key(&'a [u8]),
    Index(u64),
}

/// Computes the jq path for the `slot`-th child of `parent`, including
/// primitive children that don't carry records. Returns `None` if the
/// parent isn't a container or `slot` is out of range. Decodes
/// JSON-string escapes in primitive keys so the segment formatter sees
/// the same UTF-8 bytes it would for a record-bearing object member.
pub fn compute_child_path(doc: &Document, parent: u32, slot: u32) -> Option<Vec<u8>> {
    let open = ContainerOpen::new(doc, parent)?;
    let mut next_skippable = open.initial_next_skippable;
    let source = open.source;
    let close = open.close_byte();
    let mut pos: usize = 1;
    skip_ws(source, &mut pos);
    if pos < source.len() && source[pos] == close {
        return None;
    }

    let mut current_slot: u32 = 0;
    loop {
        // Object key span — capture the raw byte range so the segment
        // renderer can decode primitive keys inline; record-bearing
        // members get their already-decoded bytes from the keys arena.
        let mut raw_key_offset: usize = 0;
        let mut raw_key_length: usize = 0;
        if open.kind == NodeKind::Object {
            let key_start = pos;
            if !skip_string(source, &mut pos) { return None; }
            raw_key_offset = key_start + 1;
            raw_key_length = (pos - 1) - (key_start + 1);
            skip_ws(source, &mut pos);
            if pos >= source.len() || source[pos] != b':' { return None; }
            pos += 1;
            skip_ws(source, &mut pos);
        }

        let is_skippable = open.at_skippable(doc, pos, next_skippable);

        if current_slot == slot {
            let mut out = compute_path(doc, parent);
            // Strip trailing "." for the root, since segment renderers
            // emit their own leading dot/bracket.
            if out == b"." { out.clear(); }
            match open.kind {
                NodeKind::Object => {
                    write_object_segment(doc, source, raw_key_offset, raw_key_length,
                        is_skippable, next_skippable, &mut out)?;
                }
                NodeKind::Array => {
                    let idx = current_slot as u64;
                    let needs_dot = out.is_empty();
                    push_index_segment(idx, &mut out, needs_dot);
                }
                _ => return None,
            }
            return Some(out);
        }

        if is_skippable {
            let r = doc.record(next_skippable)?;
            pos += r.length as usize;
            next_skippable = open.advance_skippable(next_skippable, r.subtree_size);
        } else {
            skip_inline_value(source, &mut pos);
        }
        current_slot += 1;

        skip_ws(source, &mut pos);
        if pos >= source.len() { return None; }
        match source[pos] {
            b',' => { pos += 1; skip_ws(source, &mut pos); continue; }
            c if c == close => return None,
            _ => return None,
        }
    }
}

/// Appends the object-member segment for the slot currently positioned
/// at by the iterator. For record-bearing children the key bytes come
/// from the already-decoded keys arena; for primitive children we
/// decode the raw source bytes via `decode_json_string_inner`.
fn write_object_segment(
    doc: &Document,
    source: &[u8],
    raw_key_offset: usize,
    raw_key_length: usize,
    is_skippable: bool,
    next_skippable: u32,
    out: &mut Vec<u8>,
) -> Option<()> {
    if is_skippable {
        let r = doc.record(next_skippable)?;
        let s = r.key_or_index as usize;
        let e = s.checked_add(r.key_length as usize)?;
        let key = doc.keys().get(s..e)?;
        push_key_segment(key, out);
    } else {
        let raw = source.get(raw_key_offset..raw_key_offset + raw_key_length)?;
        let decoded = decode_json_string_inner(raw)?;
        push_key_segment(&decoded, out);
    }
    Some(())
}

fn push_key_segment(key: &[u8], out: &mut Vec<u8>) {
    if is_simple_identifier(key) {
        out.push(b'.');
        out.extend_from_slice(key);
    } else {
        if out.is_empty() {
            out.push(b'.');
        }
        out.push(b'[');
        json_string_into(key, out);
        out.push(b']');
    }
}

fn push_index_segment(idx: u64, out: &mut Vec<u8>, needs_leading_dot: bool) {
    if needs_leading_dot {
        out.push(b'.');
    }
    out.push(b'[');
    push_decimal(out, idx);
    out.push(b']');
}

pub fn compute_path(doc: &Document, node: u32) -> Vec<u8> {
    let mut segments: Vec<Segment> = Vec::with_capacity(8);
    let mut cur = node;
    loop {
        let Some(r) = doc.record(cur) else { break };
        if r.parent == u32::MAX {
            break;
        }
        if r.flags & FLAG_OBJECT_MEMBER != 0 {
            let s = r.key_or_index as usize;
            if let Some(e) = s.checked_add(r.key_length as usize) {
                if let Some(key) = doc.keys().get(s..e) {
                    segments.push(Segment::Key(key));
                }
            }
        } else if r.flags & FLAG_ARRAY_ELEMENT != 0 {
            segments.push(Segment::Index(r.key_or_index));
        }
        cur = r.parent;
    }

    if segments.is_empty() {
        return b".".to_vec();
    }

    let mut out: Vec<u8> = Vec::with_capacity(segments.len() * 8 + 2);
    let last = segments.len() - 1;
    for (i, seg) in segments.iter().rev().enumerate() {
        match seg {
            Segment::Key(key) => {
                if is_simple_identifier(key) {
                    out.push(b'.');
                    out.extend_from_slice(key);
                } else {
                    if i == 0 {
                        out.push(b'.');
                    }
                    out.push(b'[');
                    json_string_into(key, &mut out);
                    out.push(b']');
                }
            }
            Segment::Index(idx) => {
                if i == 0 {
                    out.push(b'.');
                }
                out.push(b'[');
                push_decimal(&mut out, *idx);
                out.push(b']');
            }
        }
        let _ = last; // suppress unused-variable lint if we ever drop the use
    }
    out
}

fn is_simple_identifier(s: &[u8]) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    s.iter().skip(1).all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

fn json_string_into(s: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for &c in s {
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            c if c < 0x20 => {
                let mut buf = [0u8; 6];
                let s = format_hex_escape(c, &mut buf);
                out.extend_from_slice(s);
            }
            _ => out.push(c),
        }
    }
    out.push(b'"');
}

fn format_hex_escape(c: u8, buf: &mut [u8; 6]) -> &[u8] {
    let hex = b"0123456789abcdef";
    buf[0] = b'\\';
    buf[1] = b'u';
    buf[2] = b'0';
    buf[3] = b'0';
    buf[4] = hex[(c >> 4) as usize];
    buf[5] = hex[(c & 0x0F) as usize];
    &buf[..]
}

fn push_decimal(out: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let start = out.len();
    while n > 0 {
        out.push(b'0' + (n % 10) as u8);
        n /= 10;
    }
    out[start..].reverse();
}
