//! Low-level byte-scanners over JSON source spans. Used by the
//! evaluator (when surfacing primitive children of a hybrid-gate
//! container) and by the FFI batch entry points (when interleaving
//! record-bearing children with primitives for the UI).
//!
//! These primitives are byte-level and value-agnostic — they advance
//! a `pos` cursor through a slice of source bytes. Higher-level wrappers
//! decide what to do at each child boundary (emit a `Value`, fill an
//! `EngineChildMeta`, etc.).

#[inline]
pub fn skip_ws(src: &[u8], pos: &mut usize) {
    while *pos < src.len() {
        match src[*pos] {
            b' ' | b'\t' | b'\r' | b'\n' => *pos += 1,
            _ => break,
        }
    }
}

/// Walks past a JSON string literal. `*pos` must point at the opening
/// quote on entry; on success it points one past the closing quote.
pub fn skip_string(src: &[u8], pos: &mut usize) -> bool {
    if *pos >= src.len() || src[*pos] != b'"' { return false; }
    *pos += 1;
    while *pos < src.len() {
        match src[*pos] {
            b'"' => { *pos += 1; return true; }
            b'\\' => {
                *pos += 1;
                if *pos >= src.len() { return false; }
                if src[*pos] == b'u' {
                    *pos += 1;
                    if *pos + 4 > src.len() { return false; }
                    *pos += 4;
                } else {
                    *pos += 1;
                }
            }
            _ => *pos += 1,
        }
    }
    false
}

/// Parses a JSON string and returns a view of its decoded contents.
/// The fast path — and the typical one for object keys, which are
/// usually bare ASCII identifiers — borrows directly from `src` with
/// no allocation. The slow path (escape sequences encountered)
/// allocates a decoded `Vec<u8>` via [`parse_string_decoded`].
///
/// Either way, `*pos` is advanced past the closing quote on success.
/// Returns `None` on malformed input.
pub fn parse_string_view<'a>(src: &'a [u8], pos: &mut usize) -> Option<std::borrow::Cow<'a, [u8]>> {
    if *pos >= src.len() || src[*pos] != b'"' { return None; }
    let start = *pos + 1;
    let mut i = start;
    while i < src.len() {
        match src[i] {
            b'"' => {
                *pos = i + 1;
                return Some(std::borrow::Cow::Borrowed(&src[start..i]));
            }
            b'\\' => {
                // Escape encountered — rewind to the opening quote and
                // re-parse with the decoder. The fast path has already
                // confirmed the bytes up to here are escape-free, so
                // the decoder's escape branch fires immediately.
                *pos = start - 1;
                return parse_string_decoded(src, pos).map(std::borrow::Cow::Owned);
            }
            _ => i += 1,
        }
    }
    None
}

/// Parses a JSON string literal and returns its decoded bytes (UTF-8).
/// Advances `*pos` past the closing quote. Returns `None` on malformed
/// input. Used to compare an object key against a target name without
/// allocating per-character.
pub fn parse_string_decoded(src: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    if *pos >= src.len() || src[*pos] != b'"' { return None; }
    *pos += 1;
    let mut out: Vec<u8> = Vec::new();
    while *pos < src.len() {
        match src[*pos] {
            b'"' => { *pos += 1; return Some(out); }
            b'\\' => {
                *pos += 1;
                if *pos >= src.len() { return None; }
                match src[*pos] {
                    b'"'  => { out.push(b'"');  *pos += 1; }
                    b'\\' => { out.push(b'\\'); *pos += 1; }
                    b'/'  => { out.push(b'/');  *pos += 1; }
                    b'b'  => { out.push(0x08); *pos += 1; }
                    b'f'  => { out.push(0x0C); *pos += 1; }
                    b'n'  => { out.push(b'\n'); *pos += 1; }
                    b'r'  => { out.push(b'\r'); *pos += 1; }
                    b't'  => { out.push(b'\t'); *pos += 1; }
                    b'u' => {
                        *pos += 1;
                        let high = parse_hex4(src, pos)?;
                        let code = if (0xD800..=0xDBFF).contains(&high)
                            && *pos + 2 <= src.len()
                            && src[*pos] == b'\\'
                            && src[*pos + 1] == b'u'
                        {
                            *pos += 2;
                            let low = parse_hex4(src, pos)?;
                            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                        } else {
                            high
                        };
                        if let Some(ch) = char::from_u32(code) {
                            let mut buf = [0u8; 4];
                            let s = ch.encode_utf8(&mut buf);
                            out.extend_from_slice(s.as_bytes());
                        }
                    }
                    _ => return None,
                }
            }
            c => { out.push(c); *pos += 1; }
        }
    }
    None
}

/// Decodes a JSON string's inter-quote bytes (no surrounding `"`).
/// Produces UTF-8 — invalid `\uXXXX` codepoints are dropped silently
/// rather than poisoning the output. Returns `None` on a malformed
/// escape sequence (e.g. trailing `\` or non-hex `\uXXXX`).
pub fn decode_json_string_inner(inner: &[u8]) -> Option<Vec<u8>> {
    if !inner.contains(&b'\\') {
        return Some(inner.to_vec());
    }
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut pos = 0usize;
    while pos < inner.len() {
        match inner[pos] {
            b'\\' => {
                pos += 1;
                if pos >= inner.len() { return None; }
                match inner[pos] {
                    b'"'  => { out.push(b'"');  pos += 1; }
                    b'\\' => { out.push(b'\\'); pos += 1; }
                    b'/'  => { out.push(b'/');  pos += 1; }
                    b'b'  => { out.push(0x08); pos += 1; }
                    b'f'  => { out.push(0x0C); pos += 1; }
                    b'n'  => { out.push(b'\n'); pos += 1; }
                    b'r'  => { out.push(b'\r'); pos += 1; }
                    b't'  => { out.push(b'\t'); pos += 1; }
                    b'u' => {
                        pos += 1;
                        let high = parse_hex4(inner, &mut pos)?;
                        let code = if (0xD800..=0xDBFF).contains(&high)
                            && pos + 2 <= inner.len()
                            && inner[pos] == b'\\'
                            && inner[pos + 1] == b'u'
                        {
                            pos += 2;
                            let low = parse_hex4(inner, &mut pos)?;
                            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                        } else {
                            high
                        };
                        if let Some(ch) = char::from_u32(code) {
                            let mut buf = [0u8; 4];
                            let s = ch.encode_utf8(&mut buf);
                            out.extend_from_slice(s.as_bytes());
                        }
                    }
                    _ => return None,
                }
            }
            c => { out.push(c); pos += 1; }
        }
    }
    Some(out)
}

pub fn parse_hex4(src: &[u8], pos: &mut usize) -> Option<u32> {
    if *pos + 4 > src.len() { return None; }
    let mut code = 0u32;
    for _ in 0..4 {
        let v = match src[*pos] {
            b'0'..=b'9' => (src[*pos] - b'0') as u32,
            b'a'..=b'f' => (src[*pos] - b'a' + 10) as u32,
            b'A'..=b'F' => (src[*pos] - b'A' + 10) as u32,
            _ => return None,
        };
        code = code * 16 + v;
        *pos += 1;
    }
    Some(code)
}

/// Advances past a JSON value at `*pos` without materialising it.
pub fn skip_inline_value(src: &[u8], pos: &mut usize) {
    if *pos >= src.len() { return; }
    match src[*pos] {
        b'"' => { let _ = skip_string(src, pos); }
        b't' => *pos += 4,
        b'f' => *pos += 5,
        b'n' => *pos += 4,
        b'-' | b'0'..=b'9' => skip_number(src, pos),
        b'{' | b'[' => skip_container(src, pos),
        _ => *pos = src.len(),
    }
}

pub fn skip_number(src: &[u8], pos: &mut usize) {
    if *pos >= src.len() { return; }
    if src[*pos] == b'-' { *pos += 1; }
    while *pos < src.len() && matches!(src[*pos], b'0'..=b'9') { *pos += 1; }
    if *pos < src.len() && src[*pos] == b'.' {
        *pos += 1;
        while *pos < src.len() && matches!(src[*pos], b'0'..=b'9') { *pos += 1; }
    }
    if *pos < src.len() && (src[*pos] == b'e' || src[*pos] == b'E') {
        *pos += 1;
        if *pos < src.len() && (src[*pos] == b'+' || src[*pos] == b'-') { *pos += 1; }
        while *pos < src.len() && matches!(src[*pos], b'0'..=b'9') { *pos += 1; }
    }
}

pub fn skip_container(src: &[u8], pos: &mut usize) {
    if *pos >= src.len() { return; }
    let open = src[*pos];
    let close = if open == b'{' { b'}' } else { b']' };
    let mut depth = 1usize;
    *pos += 1;
    while *pos < src.len() && depth > 0 {
        match src[*pos] {
            b'"' => { let _ = skip_string(src, pos); }
            c if c == open => { depth += 1; *pos += 1; }
            c if c == close => { depth -= 1; *pos += 1; }
            _ => *pos += 1,
        }
    }
}

/// Returns the JSON kind byte at `*pos` (or 0=Null on bad input).
/// Used by FFI batch traversal to label primitive children. Doesn't
/// advance `*pos`.
pub fn peek_kind(src: &[u8], pos: usize) -> u8 {
    use crate::document::NodeKind;
    if pos >= src.len() { return NodeKind::Null as u8; }
    match src[pos] {
        b'"' => NodeKind::String as u8,
        b't' | b'f' => NodeKind::Bool as u8,
        b'n' => NodeKind::Null as u8,
        b'-' | b'0'..=b'9' => NodeKind::Number as u8,
        b'{' => NodeKind::Object as u8,
        b'[' => NodeKind::Array as u8,
        _ => NodeKind::Null as u8,
    }
}
