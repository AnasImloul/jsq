//! Hand-written byte-level JSON parser. Builds the sidecar index in
//! one pass over the input via recursive descent.
//!
//! # Output sinks
//!
//! Records and decoded keys are written into caller-supplied
//! [`RecordSink`] / [`KeySink`] handles. Both are file-backed pwrite
//! streams with small in-memory buffers — peak parse-time RAM stays
//! bounded by the buffer sizes regardless of how big the JSON is.
//!
//! # Hybrid emit-gate
//!
//! Only containers and strings whose source span is at least
//! [`FAT_STRING_THRESHOLD`] bytes get a record. Smaller primitives
//! live only in the source mmap and are reconstructed on demand by
//! source-scanning their parent. Object keys are decoded into the
//! keys arena speculatively for every member, and rolled back via
//! [`KeySink::truncate`] when the member's value turns out to be a
//! primitive that doesn't get a record.
//!
//! # Pre-order layout
//!
//! Records are emitted in document pre-order, so a container's
//! descendants occupy `[id+1 .. id+subtree_size)` in the records
//! array. `subtree_size` isn't known until the container closes, so
//! it's patched in via [`RecordSink::patch`] — for the few containers
//! whose record has already been flushed by then (root, top-level
//! arrays of multi-GB inputs) `patch` does a pread + modify + pwrite;
//! everything else is patched in-place in the sink buffer.

use std::fs::File;
use std::os::unix::fs::FileExt;

use crate::document::{
    NodeKind, NodeRecord, FAT_STRING_THRESHOLD, FLAG_ARRAY_ELEMENT, FLAG_OBJECT_MEMBER, NULL_NODE,
};
use crate::error::EngineError;

const NODE_RECORD_BYTES: usize = std::mem::size_of::<NodeRecord>();

/// Append-only writer for [`NodeRecord`]s backed by a file plus a
/// small in-memory buffer. Records `[first_in_buffer .. next_id)`
/// live in the buffer; older records `[0 .. first_in_buffer)` have
/// been pwritten to disk at `base_offset + id * size_of::<NodeRecord>()`.
/// When the buffer fills, the oldest half is flushed in a single
/// pwrite.
///
/// [`patch`](Self::patch) updates a previously-pushed record (used for
/// `subtree_size`/`length`/`child_count` on container close). Records
/// that are still in the buffer are modified in place; flushed
/// records take a `pread + modify + pwrite` round-trip. Sizing the
/// buffer to comfortably exceed any short subtree's record count
/// keeps the disk-patch path off the hot path.
pub struct RecordSink {
    file: File,
    base_offset: u64,
    next_id: u32,
    first_in_buffer: u32,
    buffer: Vec<NodeRecord>,
    capacity: usize,
}

unsafe impl Send for RecordSink {}

impl RecordSink {
    /// `file` must be open for read+write. Records are written at
    /// `base_offset + id * size_of::<NodeRecord>()`. `capacity` bounds
    /// the in-memory buffer; smaller saves RAM, larger reduces the
    /// chance of a patch landing on a flushed record.
    pub fn new(file: File, base_offset: u64, capacity: usize) -> Self {
        Self {
            file,
            base_offset,
            next_id: 0,
            first_in_buffer: 0,
            buffer: Vec::with_capacity(capacity),
            capacity,
        }
    }

    #[inline]
    pub fn push(&mut self, record: NodeRecord) -> Result<u32, EngineError> {
        if self.next_id == u32::MAX {
            return Err(EngineError::parse(
                0,
                "engine supports up to u32::MAX - 1 records",
            ));
        }
        if self.buffer.len() >= self.capacity {
            self.flush_oldest_half()?;
        }
        self.buffer.push(record);
        let id = self.next_id;
        self.next_id += 1;
        Ok(id)
    }

    /// Modifies the record with id `id`. If still in the buffer,
    /// modifies in place; otherwise pread + closure + pwrite at the
    /// record's disk offset.
    pub fn patch<F>(&mut self, id: u32, f: F) -> Result<(), EngineError>
    where
        F: FnOnce(&mut NodeRecord),
    {
        debug_assert!(id < self.next_id);
        if id >= self.first_in_buffer {
            let idx = (id - self.first_in_buffer) as usize;
            f(&mut self.buffer[idx]);
            return Ok(());
        }
        // On disk: read, patch, write.
        let offset = self.base_offset + id as u64 * NODE_RECORD_BYTES as u64;
        let mut rec = std::mem::MaybeUninit::<NodeRecord>::uninit();
        // SAFETY: pread fills the entire 48-byte slice from disk; the
        // record was previously written by `flush_oldest_half`, which
        // serialises the fully-initialised buffer entry. The
        // `assume_init` below is therefore reading a value that was
        // valid `NodeRecord` at write time.
        let read_buf = unsafe {
            std::slice::from_raw_parts_mut(
                rec.as_mut_ptr() as *mut u8,
                NODE_RECORD_BYTES,
            )
        };
        self.file
            .read_exact_at(read_buf, offset)
            .map_err(|e| EngineError::Io(format!("pread record for patch: {}", e)))?;
        let mut rec = unsafe { rec.assume_init() };
        f(&mut rec);
        let write_buf = unsafe {
            std::slice::from_raw_parts(
                &rec as *const NodeRecord as *const u8,
                NODE_RECORD_BYTES,
            )
        };
        self.file
            .write_all_at(write_buf, offset)
            .map_err(|e| EngineError::Io(format!("pwrite patched record: {}", e)))?;
        Ok(())
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.next_id as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.next_id == 0
    }

    fn flush_oldest_half(&mut self) -> Result<(), EngineError> {
        let n = (self.capacity / 2).min(self.buffer.len());
        if n == 0 {
            return Ok(());
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self.buffer.as_ptr() as *const u8,
                n * NODE_RECORD_BYTES,
            )
        };
        let offset = self.base_offset + self.first_in_buffer as u64 * NODE_RECORD_BYTES as u64;
        self.file
            .write_all_at(bytes, offset)
            .map_err(|e| EngineError::Io(format!("pwrite records: {}", e)))?;
        self.buffer.drain(..n);
        self.first_in_buffer += n as u32;
        Ok(())
    }

    /// Flushes any remaining buffered records to disk and returns the
    /// underlying file (positioned at `base_offset + len * 48` after the
    /// records section). Caller is responsible for closing or further
    /// writing.
    pub fn finish(self) -> Result<File, EngineError> {
        let n = self.buffer.len();
        if n > 0 {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    self.buffer.as_ptr() as *const u8,
                    n * NODE_RECORD_BYTES,
                )
            };
            let offset = self.base_offset + self.first_in_buffer as u64 * NODE_RECORD_BYTES as u64;
            self.file
                .write_all_at(bytes, offset)
                .map_err(|e| EngineError::Io(format!("pwrite final records: {}", e)))?;
        }
        Ok(self.file)
    }
}

/// Append-only byte writer for the keys arena, backed by a file plus
/// a buffer. Pure append, with one exception: [`truncate`](Self::truncate)
/// rolls back to a previously-recorded length so the parser can
/// discard a speculatively-decoded key when its value turned out to
/// be a non-record primitive.
///
/// `truncate` always targets a position written *during* the current
/// key's parse — i.e. immediately after a push, before any flush. As
/// long as the buffer is larger than any realistic single key, the
/// rollback target stays in-buffer.
pub struct KeySink {
    file: File,
    file_bytes: u64,
    buffer: Vec<u8>,
    capacity: usize,
}

unsafe impl Send for KeySink {}

impl KeySink {
    pub fn new(file: File, capacity: usize) -> Self {
        Self {
            file,
            file_bytes: 0,
            buffer: Vec::with_capacity(capacity),
            capacity,
        }
    }

    #[inline]
    pub fn push(&mut self, byte: u8) -> Result<(), EngineError> {
        if self.buffer.len() >= self.capacity {
            self.flush_buffer()?;
        }
        self.buffer.push(byte);
        Ok(())
    }

    #[inline]
    pub fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<(), EngineError> {
        if self.buffer.len() + bytes.len() > self.capacity {
            self.flush_buffer()?;
            if bytes.len() > self.capacity {
                // Slice larger than a whole buffer — write directly.
                self.file
                    .write_all_at(bytes, self.file_bytes)
                    .map_err(|e| EngineError::Io(format!("pwrite keys: {}", e)))?;
                self.file_bytes += bytes.len() as u64;
                return Ok(());
            }
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn flush_buffer(&mut self) -> Result<(), EngineError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.file
            .write_all_at(&self.buffer, self.file_bytes)
            .map_err(|e| EngineError::Io(format!("pwrite keys: {}", e)))?;
        self.file_bytes += self.buffer.len() as u64;
        self.buffer.clear();
        Ok(())
    }

    #[inline]
    pub fn len(&self) -> usize {
        (self.file_bytes as usize) + self.buffer.len()
    }

    /// Rolls the sink back to a previously-recorded length. The target
    /// must be at or past `file_bytes` — i.e. within the live buffer.
    /// The parser only calls this immediately after a key push, before
    /// any flush could have moved it to disk.
    #[inline]
    pub fn truncate(&mut self, new_len: usize) {
        let new_len = new_len as u64;
        debug_assert!(
            new_len >= self.file_bytes,
            "key sink truncate below flushed bytes — increase buffer capacity"
        );
        let buffer_new_len = (new_len - self.file_bytes) as usize;
        self.buffer.truncate(buffer_new_len);
    }

    /// Flushes any remaining bytes to disk and returns the file.
    pub fn finish(mut self) -> Result<File, EngineError> {
        self.flush_buffer()?;
        Ok(self.file)
    }
}

pub struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    records: RecordSink,
    keys: KeySink,
}

#[derive(Copy, Clone)]
enum KeyInfo {
    Root,
    ObjectMember { key_offset: u64, key_length: u32 },
    ArrayElement { index: u32 },
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8], records: RecordSink, keys: KeySink) -> Self {
        Self { data, pos: 0, records, keys }
    }

    /// Parses the input, returning the populated sinks so the caller can
    /// observe their final lengths and finalise the sidecar.
    pub fn parse(mut self) -> Result<(RecordSink, KeySink), EngineError> {
        self.skip_ws();
        if self.pos >= self.data.len() {
            return Err(EngineError::parse(0, "expected JSON value"));
        }
        // The root must be a container or fat string (something that
        // produces a record), since the FFI and tests assume `root_id
        // == 0`. JSON files with a bare primitive at the root would
        // need a synthesised record-zero, which the engine doesn't
        // currently support.
        let root = self.parse_value(NULL_NODE, KeyInfo::Root)?;
        if root.is_none() {
            return Err(EngineError::parse(0, "bare primitive root not supported"));
        }
        self.skip_ws();
        if self.pos < self.data.len() {
            return Err(EngineError::parse(self.pos, "trailing content after root value"));
        }
        Ok((self.records, self.keys))
    }

    fn skip_ws(&mut self) {
        while self.pos < self.data.len() {
            match self.data[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                _ => break,
            }
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8, msg: &'static str) -> Result<(), EngineError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(EngineError::parse(self.pos, msg))
        }
    }

    /// Parses one value. Under the hybrid emit-gate, the return is
    /// `Some(id)` when the value got a record (containers always do;
    /// strings do iff their source span >= `FAT_STRING_THRESHOLD`) and
    /// `None` for small primitives (numbers, bools, nulls, short
    /// strings) — the parser advances past their bytes but emits no
    /// record. Callers (parse_object / parse_array) bump child_count
    /// regardless and roll back any speculatively-decoded key when the
    /// value didn't emit.
    fn parse_value(&mut self, parent: u32, key: KeyInfo) -> Result<Option<u32>, EngineError> {
        self.skip_ws();
        let start = self.pos;
        let c = self.peek().ok_or_else(|| EngineError::parse(start, "expected value"))?;
        match c {
            b'{' => self.parse_object(parent, key, start).map(Some),
            b'[' => self.parse_array(parent, key, start).map(Some),
            b'"' => self.parse_string_value(parent, key, start),
            b't' => self.parse_literal(start, b"true"),
            b'f' => self.parse_literal(start, b"false"),
            b'n' => self.parse_literal(start, b"null"),
            b'-' | b'0'..=b'9' => self.parse_number(start),
            _ => Err(EngineError::parse(start, "unexpected character at start of value")),
        }
    }

    fn parse_object(&mut self, parent: u32, key: KeyInfo, start: usize) -> Result<u32, EngineError> {
        let node = self.add_node(start as u64, 0, NodeKind::Object, parent, key)?;
        // Pre-order parsing means every descendant emits a record before
        // we return here, so `records.len() - node` is exactly the
        // subtree size including self.
        let records_at_open = node;
        self.pos += 1; // consume '{'
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            let subtree_size = self.records.len() as u32 - records_at_open;
            let length = (self.pos - start) as u64;
            self.records.patch(node, |r| {
                r.length = length;
                r.subtree_size = subtree_size;
            })?;
            return Ok(node);
        }
        let mut child_count: u32 = 0;
        loop {
            self.skip_ws();
            // Speculatively decode the key into the arena. If the value
            // turns out to be a small primitive (no record), roll back
            // the key bytes — we only retain keys for record-bearing
            // members.
            let key_offset = self.keys.len() as u64;
            self.parse_string_into_keys()?;
            let key_len_bytes = self.keys.len() - key_offset as usize;
            // Decoded key length is recorded as u32 — JSON allows keys
            // of any length, but anything past 4 GiB is degenerate input
            // we'd rather refuse than silently truncate.
            if key_len_bytes > u32::MAX as usize {
                return Err(EngineError::parse(
                    self.pos,
                    "object key exceeds u32::MAX bytes",
                ));
            }
            let key_length = key_len_bytes as u32;
            self.skip_ws();
            self.expect(b':', "expected ':' after object key")?;
            let child = self.parse_value(
                node,
                KeyInfo::ObjectMember { key_offset, key_length },
            )?;
            if child.is_none() {
                // Primitive value: discard the speculatively-decoded key.
                self.keys.truncate(key_offset as usize);
            }
            child_count += 1;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(EngineError::parse(self.pos, "expected ',' or '}' in object")),
            }
        }
        let subtree_size = self.records.len() as u32 - records_at_open;
        let length = (self.pos - start) as u64;
        self.records.patch(node, |r| {
            r.child_count = child_count;
            r.length = length;
            r.subtree_size = subtree_size;
        })?;
        // Object closed — pulse the parse-progress beacon so the UI's
        // determinate progress bar can advance. Single relaxed atomic
        // store; cost is negligible relative to the parsing work.
        crate::progress::report_parse_progress(self.pos as u64);
        Ok(node)
    }

    fn parse_array(&mut self, parent: u32, key: KeyInfo, start: usize) -> Result<u32, EngineError> {
        let node = self.add_node(start as u64, 0, NodeKind::Array, parent, key)?;
        let records_at_open = node;
        self.pos += 1; // consume '['
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            let subtree_size = self.records.len() as u32 - records_at_open;
            let length = (self.pos - start) as u64;
            self.records.patch(node, |r| {
                r.length = length;
                r.subtree_size = subtree_size;
            })?;
            return Ok(node);
        }
        let mut child_count: u32 = 0;
        let mut index: u32 = 0;
        loop {
            self.skip_ws();
            let _child = self.parse_value(node, KeyInfo::ArrayElement { index })?;
            child_count += 1;
            index += 1;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(EngineError::parse(self.pos, "expected ',' or ']' in array")),
            }
        }
        let subtree_size = self.records.len() as u32 - records_at_open;
        let length = (self.pos - start) as u64;
        self.records.patch(node, |r| {
            r.child_count = child_count;
            r.length = length;
            r.subtree_size = subtree_size;
        })?;
        crate::progress::report_parse_progress(self.pos as u64);
        Ok(node)
    }

    fn parse_string_value(
        &mut self,
        parent: u32,
        key: KeyInfo,
        start: usize,
    ) -> Result<Option<u32>, EngineError> {
        self.consume_string()?;
        let length = (self.pos - start) as u64;
        if length >= FAT_STRING_THRESHOLD as u64 {
            self.add_node(start as u64, length, NodeKind::String, parent, key).map(Some)
        } else {
            // Small string — no record. The parser has already advanced
            // past the closing quote; the parent's source-scan will
            // re-discover this value's offset/length when needed.
            Ok(None)
        }
    }

    fn parse_literal(&mut self, _start: usize, lit: &[u8]) -> Result<Option<u32>, EngineError> {
        if self.data.get(self.pos..self.pos + lit.len()) != Some(lit) {
            return Err(EngineError::parse(self.pos, "invalid literal"));
        }
        self.pos += lit.len();
        // Literals never emit records under the hybrid gate.
        Ok(None)
    }

    fn parse_number(&mut self, _start: usize) -> Result<Option<u32>, EngineError> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(EngineError::parse(self.pos, "expected digit in number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(EngineError::parse(self.pos, "expected digit after '.'"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(EngineError::parse(self.pos, "expected digit in exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        // Numbers never emit records under the hybrid gate.
        Ok(None)
    }

    /// Walks past a JSON string. Leaves pos one past the closing quote.
    fn consume_string(&mut self) -> Result<(), EngineError> {
        if self.peek() != Some(b'"') {
            return Err(EngineError::parse(self.pos, "expected '\"'"));
        }
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err(EngineError::parse(self.pos, "unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        None => return Err(EngineError::parse(self.pos, "unterminated escape")),
                        Some(b'u') => {
                            self.pos += 1;
                            for _ in 0..4 {
                                match self.peek() {
                                    Some(c) if c.is_ascii_hexdigit() => self.pos += 1,
                                    _ => return Err(EngineError::parse(self.pos, "invalid \\u escape")),
                                }
                            }
                        }
                        Some(_) => self.pos += 1,
                    }
                }
                Some(c) if c < 0x20 => {
                    return Err(EngineError::parse(self.pos, "control character in string"));
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Reads a JSON string and decodes it into `self.keys`. Leaves pos
    /// past the closing quote.
    fn parse_string_into_keys(&mut self) -> Result<(), EngineError> {
        if self.peek() != Some(b'"') {
            return Err(EngineError::parse(self.pos, "expected string key"));
        }
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err(EngineError::parse(self.pos, "unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"')  => { self.keys.push(b'"')?;  self.pos += 1; }
                        Some(b'\\') => { self.keys.push(b'\\')?; self.pos += 1; }
                        Some(b'/')  => { self.keys.push(b'/')?;  self.pos += 1; }
                        Some(b'b')  => { self.keys.push(0x08)?;  self.pos += 1; }
                        Some(b'f')  => { self.keys.push(0x0C)?;  self.pos += 1; }
                        Some(b'n')  => { self.keys.push(b'\n')?; self.pos += 1; }
                        Some(b'r')  => { self.keys.push(b'\r')?; self.pos += 1; }
                        Some(b't')  => { self.keys.push(b'\t')?; self.pos += 1; }
                        Some(b'u') => {
                            self.pos += 1;
                            let high = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&high) {
                                if self.peek() != Some(b'\\') {
                                    return Err(EngineError::parse(self.pos, "expected low surrogate"));
                                }
                                self.pos += 1;
                                if self.peek() != Some(b'u') {
                                    return Err(EngineError::parse(self.pos, "expected \\u"));
                                }
                                self.pos += 1;
                                let low = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(EngineError::parse(self.pos, "invalid low surrogate"));
                                }
                                let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                                self.push_utf8(code)?;
                            } else if (0xDC00..=0xDFFF).contains(&high) {
                                return Err(EngineError::parse(self.pos, "lone low surrogate"));
                            } else {
                                self.push_utf8(high)?;
                            }
                        }
                        _ => return Err(EngineError::parse(self.pos, "invalid escape")),
                    }
                }
                Some(c) if c < 0x20 => {
                    return Err(EngineError::parse(self.pos, "control character in string"));
                }
                Some(c) => {
                    self.keys.push(c)?;
                    self.pos += 1;
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, EngineError> {
        let mut code = 0u32;
        for _ in 0..4 {
            let c = self.peek().ok_or_else(|| EngineError::parse(self.pos, "incomplete \\u escape"))?;
            let v = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return Err(EngineError::parse(self.pos, "invalid hex digit")),
            };
            code = code * 16 + v;
            self.pos += 1;
        }
        Ok(code)
    }

    fn push_utf8(&mut self, code: u32) -> Result<(), EngineError> {
        match char::from_u32(code) {
            Some(ch) => {
                let mut buf = [0u8; 4];
                let s = ch.encode_utf8(&mut buf);
                self.keys.extend_from_slice(s.as_bytes())?;
                Ok(())
            }
            None => Err(EngineError::parse(self.pos, "invalid Unicode code point")),
        }
    }

    fn add_node(
        &mut self,
        offset: u64,
        length: u64,
        kind: NodeKind,
        parent: u32,
        key: KeyInfo,
    ) -> Result<u32, EngineError> {
        // subtree_size is patched on container close; primitives never
        // patch it, so we initialize to 1 (self with no descendants).
        let mut rec = NodeRecord {
            offset,
            length,
            parent,
            subtree_size: 1,
            child_count: 0,
            key_or_index: 0,
            key_length: 0,
            kind: kind as u8,
            flags: 0,
        };
        match key {
            KeyInfo::Root => {}
            KeyInfo::ObjectMember { key_offset, key_length } => {
                rec.key_or_index = key_offset;
                rec.key_length = key_length;
                rec.flags = FLAG_OBJECT_MEMBER;
            }
            KeyInfo::ArrayElement { index } => {
                rec.key_or_index = index as u64;
                rec.flags = FLAG_ARRAY_ELEMENT;
            }
        }
        self.records.push(rec)
    }
}
