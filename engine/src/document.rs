use memmap2::Mmap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, UNIX_EPOCH};

use std::sync::Mutex;

use crate::error::EngineError;
use crate::parser::{KeySink, Parser, RecordSink};
use crate::query::index::IndexRegistry;
use crate::sidecar::{self, LoadedSidecar, SidecarHeader, SIDECAR_HEADER_SIZE, SIDECAR_VERSION};

pub const NULL_NODE: u32 = u32::MAX;

pub const FLAG_OBJECT_MEMBER: u8 = 1;
pub const FLAG_ARRAY_ELEMENT: u8 = 2;
/// Set on a child meta entry when the entry's key bytes live in the
/// source mmap (raw, between quotes) rather than in the document's
/// decoded keys arena. Used by FFI batch traversal to surface primitive
/// object members — under the hybrid emit-gate, their keys aren't
/// pre-decoded into the arena.
pub const FLAG_KEY_IN_SOURCE: u8 = 4;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Null = 0,
    Bool = 1,
    Number = 2,
    String = 3,
    Array = 4,
    Object = 5,
}

impl NodeKind {
    pub fn from_u8(b: u8) -> Self {
        match b {
            0 => Self::Null,
            1 => Self::Bool,
            2 => Self::Number,
            3 => Self::String,
            4 => Self::Array,
            5 => Self::Object,
            _ => Self::Null,
        }
    }
}

/// In-memory record describing a JSON node. Records are stored in
/// **strict source-order pre-order**: for any record at index `k` with
/// `subtree_size = n`, the records `[k+1 .. k+n]` are exactly the
/// descendants of `k` in source order. This invariant is what lets us
/// derive sibling pointers from `subtree_size` without storing them.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct NodeRecord {
    pub offset: u64,
    /// Source byte length of the value. `u64` (not `u32`) because the
    /// root container of a 10GB+ document has a length that overflows
    /// 32 bits — silent wrap there reported a 10GB root as 2.15GB and
    /// truncated the FFI's child-scan source span, hiding any children
    /// past the wrap point.
    pub length: u64,
    /// Object members: byte offset into the decoded keys arena.
    /// Array elements: 0-based slot index.
    /// `u64` because the keys arena exceeds 4 GiB on key-heavy >50 GB
    /// inputs — silent wrap there returned wrong key bytes for every
    /// record past the boundary.
    pub key_or_index: u64,
    pub parent: u32,
    /// Number of records in this subtree, *including* self. Always >= 1.
    /// For leaves and fat-string records, this is 1. For containers it
    /// is 1 + the total record count of all descendants. Bounded by
    /// the u32 record-id space; the parser refuses inputs that would
    /// push past `u32::MAX` records.
    pub subtree_size: u32,
    /// Direct child values, primitives included. Drives UI "N items"
    /// badges. Independent of `subtree_size` because primitives may not
    /// have records of their own (under the hybrid emit-gate).
    pub child_count: u32,
    /// Decoded byte length of an object key. `u32` (not `u16`) because
    /// JSON keys can legally exceed 64 KiB, and silent truncation
    /// returned an invalid key prefix that mismatched query lookups.
    pub key_length: u32,
    pub kind: u8,          // NodeKind
    pub flags: u8,         // FLAG_OBJECT_MEMBER | FLAG_ARRAY_ELEMENT
    // Trailing 6 bytes of padding inserted by `repr(C)` to keep size a
    // multiple of the u64 alignment (48).
}

const _: () = {
    assert!(std::mem::size_of::<NodeRecord>() == 48);
    assert!(std::mem::align_of::<NodeRecord>() == 8);
};

/// Strings whose source byte length (incl. quotes) is < this threshold
/// don't get records under the hybrid emit-gate. They're discovered on
/// demand by source-scanning their parent. Validated against `big.json`:
/// max string length there is 141 bytes (zero strings >= 256), so this
/// threshold is inert on that corpus and defensive against blob-heavy
/// datasets.
pub const FAT_STRING_THRESHOLD: u32 = 256;

/// Smallest source span that can produce a skippable record: `{}`,
/// `[]`, or a fat-string is at least 2 bytes long. Used to derive the
/// records-section provisioning bound from the record size, so the
/// constant is an honest upper bound rather than a hand-tuned guess.
pub const MIN_BYTES_PER_SKIPPABLE: u64 = 2;

pub struct Document {
    pub source_mmap: Mmap,
    /// Memory-mapped sidecar holding the parsed records and keys
    /// arenas. Pages are paged in lazily by the kernel as queries
    /// touch them, so peak resident memory tracks the query workload
    /// rather than the JSON file size.
    loaded: LoadedSidecar,
    parsed_this_session: bool,
    /// Background finaliser (sync + rename). Long-running callers can
    /// leave it detached; CLI tools call `wait_for_sidecar` before exit
    /// to make the cache visible to the next invocation.
    sidecar_writer: Option<JoinHandle<()>>,
    /// Foreign-key indexes built on demand via `engine_query_create_index`.
    /// Mutex (rather than RwLock) because operations are short and the
    /// FFI surface lets queries run concurrently with create/drop on
    /// background queues — keeping it simple beats squeezing read parallelism
    /// out of a struct that only ever holds a few entries.
    pub indexes: Mutex<IndexRegistry>,
}

impl Document {
    /// Opens a JSON file. If `index_dir` is provided (or the system temp
    /// dir is usable as a fallback), the parse streams directly into a
    /// memory-mapped sidecar — heap memory never holds the records, so
    /// peak RSS is bounded by the kernel's working set rather than the
    /// JSON size.
    pub fn open(source_path: &Path, index_dir: Option<&Path>) -> Result<Self, EngineError> {
        let file = File::open(source_path).map_err(|e| EngineError::Io(e.to_string()))?;
        let metadata = file.metadata().map_err(|e| EngineError::Io(e.to_string()))?;
        let source_size = metadata.len();
        if source_size == 0 {
            return Err(EngineError::Empty);
        }
        let source_mtime_ns = metadata
            .modified()
            .map_err(|e| EngineError::Io(e.to_string()))?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| EngineError::Io(e.to_string()))?
            .as_nanos() as u64;

        // SAFETY: file kept open by Mmap for its lifetime.
        let source_mmap = unsafe {
            Mmap::map(&file).map_err(|e| EngineError::Io(e.to_string()))?
        };

        // Cache hit?
        if let Some(dir) = index_dir {
            let sidecar = sidecar::sidecar_path(dir, source_path);
            if let Ok(loaded) = sidecar::try_load_sidecar(&sidecar, source_size, source_mtime_ns) {
                // Snap the beacon to "fully done at known total" so a
                // poll right after the cache hit sees a coherent 100 %
                // rather than stale state from a previous parse.
                crate::progress::reset_parse_progress(source_size);
                crate::progress::finish_parse_progress();
                return Ok(Document {
                    source_mmap,
                    loaded,
                    parsed_this_session: false,
                    sidecar_writer: None,
                    indexes: Mutex::new(IndexRegistry::default()),
                });
            }
        }

        // No `madvise` on the source mmap: macOS treats `Sequential`
        // as a hint to do large batched readahead, which on multi-GB
        // inputs serialises the parser behind whole-chunk I/O waits.
        // Demand-paging without advice gives the kernel finer-grained
        // control and noticeably smoother parse progress.

        // `resolve_work_dir` always finds a usable directory (caller-
        // supplied or system temp), so the streaming build is the
        // only path.
        let work_dir = resolve_work_dir(index_dir)
            .ok_or_else(|| EngineError::Io("no usable work directory for sidecar".into()))?;
        Self::open_streaming(
            source_path,
            source_mmap,
            source_size,
            source_mtime_ns,
            index_dir,
            &work_dir,
        )
    }

    /// Builds a fresh sidecar from the source file.
    ///
    /// Records are pwrite'd directly into the final sidecar at offset
    /// `SIDECAR_HEADER_SIZE`; keys go to a parallel temp file and are
    /// appended after the records section once the parse finishes. The
    /// pwrite-with-buffer design (rather than writing through a sparse
    /// mmap) is what keeps peak parse-time RAM bounded by the sink
    /// buffer sizes (~32 MiB total) regardless of source size, and
    /// avoids the kernel-level dirty-page-management stalls that a
    /// large sparse mmap-write triggers under macOS pressure.
    fn open_streaming(
        source_path: &Path,
        source_mmap: Mmap,
        source_size: u64,
        source_mtime_ns: u64,
        sidecar_dir: Option<&Path>,
        work_dir: &Path,
    ) -> Result<Self, EngineError> {
        use std::os::unix::fs::FileExt;

        // Beacon goes to (0, source_size) up front so the UI can
        // switch from indeterminate spinner to determinate bar
        // immediately; the parser's container-close reports advance
        // it from there.
        crate::progress::reset_parse_progress(source_size);
        std::fs::create_dir_all(work_dir).map_err(|e| EngineError::Io(e.to_string()))?;

        // Sweep orphan tmp files left behind by crashed earlier runs.
        // A normal exit removes them, but a kill between sink-flush
        // and rename leaves them on disk — they'd accumulate over
        // crashed-launch cycles otherwise.
        cleanup_orphan_streaming_tmp(work_dir);

        let final_path = sidecar_dir.map(|d| sidecar::sidecar_path(d, source_path));
        let stem = match &final_path {
            Some(p) => format!(
                "{}.{}",
                p.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("sidecar"),
                std::process::id()
            ),
            None => format!(
                "bigjson-{:016x}-{}",
                sidecar::fnv1a_hash(source_path.to_string_lossy().as_bytes()),
                std::process::id()
            ),
        };
        let tmp_path = work_dir.join(format!("{}.streaming.tmp", stem));
        let tmp_keys_path = work_dir.join(format!("{}.keys.streaming.tmp", stem));

        // The records file IS the final sidecar — records pwrite into
        // it at offset `SIDECAR_HEADER_SIZE`, leaving the first 64
        // bytes for a header that's filled in at the very end. Keys
        // go to a separate temp file because the records section's
        // final size isn't known until parse completes; once it is,
        // keys are appended in a single streamed copy.
        let records_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&tmp_path)
            .map_err(|e| EngineError::Io(format!("open tmp sidecar: {e}")))?;
        let keys_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&tmp_keys_path)
            .map_err(|e| EngineError::Io(format!("open tmp keys: {e}")))?;

        // Sink buffer sizes. Records: large enough that the vast
        // majority of containers are still in-buffer when their close
        // patches their record (only root and top-level arrays of
        // multi-GB inputs hit the disk-patch path). Keys: large enough
        // that any realistic JSON key fits within one buffer, which is
        // what makes `KeySink::truncate` (used to roll back keys for
        // primitive members) safe — it never crosses the flush
        // boundary.
        const RECORD_BUFFER_RECORDS: usize = (16 * 1024 * 1024) / std::mem::size_of::<NodeRecord>();
        const KEY_BUFFER_BYTES: usize = 16 * 1024 * 1024;

        let records_sink = RecordSink::new(
            records_file,
            SIDECAR_HEADER_SIZE as u64,
            RECORD_BUFFER_RECORDS,
        );
        let keys_sink = KeySink::new(keys_file, KEY_BUFFER_BYTES);

        // Helper thread: hint the kernel it can drop source pages
        // behind the parser via MADV_DONTNEED, in 256 MiB chunks. The
        // parser advances strictly forward through the source, so
        // already-parsed pages won't be re-touched and dropping them
        // keeps the page cache from growing without bound.
        let source_mmap_ref: &Mmap = &source_mmap;
        let parse_result: Result<(RecordSink, KeySink), EngineError> =
            std::thread::scope(|scope| {
                let stop = Arc::new(AtomicBool::new(false));
                let stop_clone = Arc::clone(&stop);
                let helper_handle = scope.spawn(move || {
                    let mut last_source_drop: usize = 0;
                    const SOURCE_DROP_CHUNK: usize = 256 * 1024 * 1024;
                    while !stop_clone.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(100));
                        let (parsed, _total) = crate::progress::current_progress();
                        let parsed = parsed as usize;
                        if parsed >= last_source_drop + SOURCE_DROP_CHUNK {
                            let drop_end = parsed - (parsed % 4096);
                            if drop_end > last_source_drop {
                                let _ = unsafe {
                                    source_mmap_ref.unchecked_advise_range(
                                        memmap2::UncheckedAdvice::DontNeed,
                                        last_source_drop,
                                        drop_end - last_source_drop,
                                    )
                                };
                                last_source_drop = drop_end;
                            }
                        }
                    }
                });
                let parser = Parser::new(&source_mmap[..], records_sink, keys_sink);
                let result = parser.parse();
                stop.store(true, Ordering::Release);
                let _ = helper_handle.join();
                result
            });

        let (records_sink, keys_sink) = match parse_result {
            Ok(t) => t,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                let _ = std::fs::remove_file(&tmp_keys_path);
                return Err(e);
            }
        };

        if records_sink.is_empty() {
            let _ = std::fs::remove_file(&tmp_path);
            let _ = std::fs::remove_file(&tmp_keys_path);
            return Err(EngineError::Empty);
        }

        let actual_node_count = records_sink.len();
        let actual_records_bytes = actual_node_count * std::mem::size_of::<NodeRecord>();
        let actual_keys_size = keys_sink.len();
        let final_keys_offset = SIDECAR_HEADER_SIZE + actual_records_bytes;
        let final_size = final_keys_offset + actual_keys_size;

        // Drain the sink buffers to disk and recover the file handles
        // for the final assembly step.
        let tmp_file = records_sink
            .finish()
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                let _ = std::fs::remove_file(&tmp_keys_path);
                e
            })?;
        let keys_file = keys_sink
            .finish()
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                let _ = std::fs::remove_file(&tmp_keys_path);
                e
            })?;

        // Diagnostic header fields (max_subtree_size, fat_string_count)
        // require one pass over the records section. Streamed off
        // disk in 16 MiB chunks rather than via a fresh mmap so peak
        // RAM stays bounded.
        let (max_subtree_size, fat_string_count) = scan_records_for_stats(
            &tmp_file,
            SIDECAR_HEADER_SIZE as u64,
            actual_node_count,
        )?;

        // Concatenate keys into the sidecar after the records section.
        // Buffered copy keeps RAM flat regardless of keys arena size.
        if actual_keys_size > 0 {
            let mut buf = vec![0u8; 16 * 1024 * 1024];
            let mut src_offset: u64 = 0;
            let mut dst_offset = final_keys_offset as u64;
            let mut remaining = actual_keys_size;
            while remaining > 0 {
                let want = remaining.min(buf.len());
                keys_file
                    .read_exact_at(&mut buf[..want], src_offset)
                    .map_err(|e| EngineError::Io(format!("read tmp keys: {}", e)))?;
                tmp_file
                    .write_all_at(&buf[..want], dst_offset)
                    .map_err(|e| EngineError::Io(format!("append keys: {}", e)))?;
                src_offset += want as u64;
                dst_offset += want as u64;
                remaining -= want;
            }
        }
        drop(keys_file);
        let _ = std::fs::remove_file(&tmp_keys_path);

        // Header.
        let header = SidecarHeader {
            magic: *b"JIDX",
            version: SIDECAR_VERSION,
            source_size,
            source_mtime_ns,
            node_count: actual_node_count as u32,
            keys_size: actual_keys_size as u64,
            records_offset: SIDECAR_HEADER_SIZE as u64,
            keys_offset: final_keys_offset as u64,
            max_subtree_size,
            fat_string_count,
            _reserved: 0,
        };
        let header_bytes = unsafe {
            std::slice::from_raw_parts(
                &header as *const SidecarHeader as *const u8,
                std::mem::size_of::<SidecarHeader>(),
            )
        };
        tmp_file
            .write_all_at(header_bytes, 0)
            .map_err(|e| EngineError::Io(format!("write sidecar header: {}", e)))?;

        // Truncate to the actually-used size.
        tmp_file
            .set_len(final_size as u64)
            .map_err(|e| EngineError::Io(format!("truncate tmp sidecar: {e}")))?;

        // Close the writer fd before opening a read-only mmap. The
        // sidecar-finaliser thread spawned below will sync_data and
        // rename into place; the Document holds the read-only mapping
        // for query-time access.
        drop(tmp_file);

        let read_file = File::open(&tmp_path)
            .map_err(|e| EngineError::Io(format!("reopen tmp sidecar: {e}")))?;
        let read_mmap = unsafe { Mmap::map(&read_file) }
            .map_err(|e| EngineError::Io(format!("mmap final sidecar: {e}")))?;
        // Random-access advice: queries jump around the records array
        // following sibling chains and subtree pointers; sequential
        // readahead would just thrash.
        let _ = read_mmap.advise(memmap2::Advice::Random);

        let loaded = LoadedSidecar {
            mmap: read_mmap,
            records_offset: SIDECAR_HEADER_SIZE,
            node_count: actual_node_count,
            keys_offset: final_keys_offset,
            keys_size: actual_keys_size,
        };

        // Background finaliser: full sync + atomic rename into the
        // user-visible sidecar dir so the next open hits the cache.
        let sidecar_writer = if let Some(final_path) = final_path.clone() {
            let tmp_path = tmp_path.clone();
            std::thread::Builder::new()
                .name("sidecar-finaliser".into())
                .spawn(move || {
                    if let Ok(f) = File::open(&tmp_path) {
                        let _ = f.sync_data();
                    }
                    let _ = std::fs::rename(&tmp_path, &final_path);
                })
                .ok()
        } else {
            None
        };

        // Parser made it to EOF — pin the beacon at 100 % so the UI's
        // determinate bar settles at full before the load finishes.
        crate::progress::finish_parse_progress();
        let doc = Document {
            source_mmap,
            loaded,
            parsed_this_session: true,
            sidecar_writer,
            indexes: Mutex::new(IndexRegistry::default()),
        };
        #[cfg(debug_assertions)]
        doc.verify_subtree_invariants();
        Ok(doc)
    }

    /// Blocks until the background sidecar finaliser (if any) has
    /// completed. Long-running callers can let it run detached; CLI
    /// tools call this before exit so the cache is on disk for the
    /// next invocation.
    pub fn wait_for_sidecar(&mut self) {
        if let Some(handle) = self.sidecar_writer.take() {
            let _ = handle.join();
        }
    }

    /// True when this open avoided parsing because a valid sidecar was
    /// found.
    pub fn loaded_from_sidecar(&self) -> bool {
        !self.parsed_this_session
    }

    pub fn records(&self) -> &[NodeRecord] {
        let loaded = &self.loaded;
        let bytes_len = loaded.node_count * std::mem::size_of::<NodeRecord>();
        let start = loaded.records_offset;
        let bytes = &loaded.mmap[start..start + bytes_len];
        // SAFETY: the sidecar loader validates that `start..start+bytes_len`
        // is in bounds, that `start` is aligned to `NodeRecord` (which is
        // `repr(C)` with 8-byte alignment), and that `bytes_len` is an
        // exact multiple of `size_of::<NodeRecord>()`.
        unsafe {
            std::slice::from_raw_parts(
                bytes.as_ptr() as *const NodeRecord,
                loaded.node_count,
            )
        }
    }

    pub fn keys(&self) -> &[u8] {
        let loaded = &self.loaded;
        &loaded.mmap[loaded.keys_offset..loaded.keys_offset + loaded.keys_size]
    }

    pub fn record(&self, id: u32) -> Option<&NodeRecord> {
        self.records().get(id as usize)
    }

    pub fn node_kind(&self, id: u32) -> NodeKind {
        self.record(id)
            .map(|r| NodeKind::from_u8(r.kind))
            .unwrap_or(NodeKind::Null)
    }

    pub fn value_bytes(&self, id: u32) -> Option<&[u8]> {
        let r = self.record(id)?;
        let start = r.offset as usize;
        let end = start.checked_add(r.length as usize)?;
        self.source_mmap.get(start..end)
    }

    pub fn key_bytes(&self, id: u32) -> Option<&[u8]> {
        let r = self.record(id)?;
        if r.flags & FLAG_OBJECT_MEMBER == 0 {
            return None;
        }
        let s = r.key_or_index as usize;
        let e = s.checked_add(r.key_length as usize)?;
        self.keys().get(s..e)
    }

    /// First child of `id` that has a record. Derives from
    /// `subtree_size` and the pre-order layout invariant: when a record
    /// at `k` has `subtree_size > 1`, its first record-bearing child is
    /// at `k + 1`. Returns `NULL_NODE` for leaves and unknown ids.
    #[inline]
    pub fn first_skippable_child(&self, id: u32) -> u32 {
        match self.record(id) {
            Some(r) if r.subtree_size > 1 => id.checked_add(1).unwrap_or(NULL_NODE),
            _ => NULL_NODE,
        }
    }

    /// Next sibling of `id` whose parent's subtree ends at
    /// `parent_subtree_end` (= `parent_id + parent.subtree_size`).
    /// Hot-loop variant: caller already knows the parent context, so
    /// this is one record load. Uses `checked_add` so a corrupt sidecar
    /// with an inflated `subtree_size` can't wrap into a small id and
    /// emit a wrong record — overflow falls through to `NULL_NODE`.
    #[inline]
    pub fn next_skippable_sibling_with_end(&self, id: u32, parent_subtree_end: u32) -> u32 {
        match self.record(id) {
            Some(r) => match id.checked_add(r.subtree_size) {
                Some(next) if next < parent_subtree_end => next,
                _ => NULL_NODE,
            },
            None => NULL_NODE,
        }
    }

    /// Stand-alone next-sibling lookup for callers that don't already
    /// have the parent. Costs one extra record load (the parent) vs.
    /// `next_skippable_sibling_with_end`.
    #[inline]
    pub fn next_skippable_sibling(&self, id: u32) -> u32 {
        let r = match self.record(id) { Some(r) => r, None => return NULL_NODE };
        if r.parent == NULL_NODE {
            return NULL_NODE; // root has no siblings
        }
        let parent = match self.record(r.parent) { Some(p) => p, None => return NULL_NODE };
        let parent_end = match r.parent.checked_add(parent.subtree_size) {
            Some(e) => e,
            None => return NULL_NODE,
        };
        match id.checked_add(r.subtree_size) {
            Some(next) if next < parent_end => next,
            _ => NULL_NODE,
        }
    }

    /// End-exclusive index of `id`'s subtree: `id + subtree_size`.
    /// Useful for setting up child iteration over a parent. Saturates
    /// to `NULL_NODE` on overflow so callers' "is this within bounds?"
    /// checks naturally reject the bogus id.
    #[inline]
    pub fn subtree_end(&self, id: u32) -> u32 {
        match self.record(id) {
            Some(r) => id.checked_add(r.subtree_size).unwrap_or(NULL_NODE),
            None => id,
        }
    }

    /// Walks the records array once and asserts the layout invariants
    /// the rest of the engine assumes:
    ///
    /// 1. Pre-order: for every container at index `k`, its descendants
    ///    occupy `[k+1, k+subtree_size)` contiguously.
    /// 2. Parent pointers point to the closest enclosing container.
    /// 3. Non-containers have `subtree_size == 1`.
    /// 4. Subtree extents fit within the records array.
    ///
    /// Debug-only: a parser bug here propagates into every traversal
    /// and would be hard to localise downstream, so we pay the O(N) cost
    /// in debug builds to catch it at the source.
    #[cfg(debug_assertions)]
    pub fn verify_subtree_invariants(&self) {
        let records = self.records();
        if records.is_empty() { return; }
        let len = records.len() as u32;

        // Stack of (id, end_exclusive) for containers whose subtree we're
        // currently inside. Pop entries whose end has been reached, then
        // the stack top is the current record's expected parent.
        let mut stack: Vec<(u32, u32)> = Vec::with_capacity(32);
        for (i, r) in records.iter().enumerate() {
            let id = i as u32;
            while let Some(&(_, end)) = stack.last() {
                if end <= id { stack.pop(); } else { break; }
            }
            let expected_parent = stack.last().map(|&(p, _)| p).unwrap_or(NULL_NODE);
            debug_assert_eq!(
                r.parent, expected_parent,
                "record {}: parent {} != expected {}",
                id, r.parent, expected_parent,
            );
            debug_assert!(r.subtree_size >= 1, "record {}: subtree_size == 0", id);
            let end = id.saturating_add(r.subtree_size);
            debug_assert!(
                end <= len,
                "record {}: subtree end {} > records.len() {}",
                id, end, len,
            );
            let kind = NodeKind::from_u8(r.kind);
            if matches!(kind, NodeKind::Object | NodeKind::Array) {
                stack.push((id, end));
            } else {
                debug_assert_eq!(
                    r.subtree_size, 1,
                    "record {}: non-container subtree_size = {}",
                    id, r.subtree_size,
                );
            }
        }
    }
}

/// Removes orphan `*.streaming.tmp` files in `dir` left behind by
/// previous BigJSON runs that crashed / were killed mid-parse. We
/// match by extension and PID-not-currently-alive so we don't race
/// with a sibling parse on a different document. Failures here are
/// best-effort — the worst case is debris persists for another run.
fn cleanup_orphan_streaming_tmp(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let our_pid = std::process::id();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.ends_with(".streaming.tmp") { continue }
        // Two name shapes (see `open_streaming`): one ends with
        // `.<pid>.streaming.tmp` (sidecar_dir set), the other starts
        // with `bigjson-<hash>-<pid>.streaming.tmp` (sidecar_dir None).
        // Both encode the pid as a decimal between dots/dashes.
        let pid = parse_streaming_tmp_pid(name);
        // If we can't recover the pid, fall back to "delete it" — a
        // stale tmp file with no pid is by definition not in use.
        let stale = match pid {
            Some(p) if p == our_pid => false,                // ours
            Some(p) => !pid_is_alive(p),
            None => true,
        };
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn parse_streaming_tmp_pid(name: &str) -> Option<u32> {
    // strip ".streaming.tmp" suffix
    let stem = name.strip_suffix(".streaming.tmp")?;
    // pid is the last dot- or dash-delimited segment
    let last_seg = stem.rsplit(|c: char| c == '.' || c == '-').next()?;
    last_seg.parse::<u32>().ok()
}

/// Checks whether a process exists, without sending it a real signal.
/// `kill(pid, 0)` returns 0 if the process exists and we have permission
/// to signal it; ESRCH means it's gone. EPERM means it exists but we
/// can't signal — treat that as "alive" to be safe.
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 { return false; }
    // SAFETY: kill with sig=0 just probes; doesn't deliver a signal.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 { return true; }
    // Errno reachable through std::io::Error::last_os_error
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EPERM) => true,
        _ => false, // ESRCH or anything else: treat as not alive
    }
}

/// Streams the records section of the just-written sidecar in 16 MiB
/// chunks, computing `(max_subtree_size, fat_string_count)` for the
/// header. We re-read from disk rather than mmap to keep heap bounded
/// regardless of file size.
fn scan_records_for_stats(
    file: &File,
    base_offset: u64,
    node_count: usize,
) -> Result<(u32, u32), EngineError> {
    use std::os::unix::fs::FileExt;
    let rec_size = std::mem::size_of::<NodeRecord>();
    // Read in chunks aligned to NodeRecord boundary so we can cast each
    // chunk slice to &[NodeRecord] and walk it directly.
    let chunk_records = (16 * 1024 * 1024) / rec_size; // ~340 K
    let chunk_bytes = chunk_records * rec_size;
    let mut buf = vec![0u8; chunk_bytes];
    let mut max_subtree: u32 = 0;
    let mut fat_strings: u32 = 0;
    let mut read = 0usize;
    while read < node_count {
        let want = (node_count - read).min(chunk_records);
        let want_bytes = want * rec_size;
        let offset = base_offset + (read as u64) * rec_size as u64;
        file.read_exact_at(&mut buf[..want_bytes], offset)
            .map_err(|e| EngineError::Io(format!("read records for stats: {}", e)))?;
        // SAFETY: NodeRecord is repr(C), POD-compatible (all primitive
        // fields), 8-byte aligned. The buffer is a fresh `Vec<u8>`
        // whose backing is 8-aligned via the global allocator. The
        // bytes were just written by the parser so they represent
        // valid `NodeRecord` values.
        let recs = unsafe {
            std::slice::from_raw_parts(buf.as_ptr() as *const NodeRecord, want)
        };
        for r in recs {
            if r.subtree_size > max_subtree {
                max_subtree = r.subtree_size;
            }
            if r.kind == NodeKind::String as u8 && r.length >= FAT_STRING_THRESHOLD as u64 {
                fat_strings += 1;
            }
        }
        read += want;
    }
    Ok((max_subtree, fat_strings))
}

/// Where to write the streaming `.tmp` sidecar. Prefers the caller's
/// `index_dir` (so tmp + final live on the same volume — rename is
/// atomic and zero-cost) and falls back to the system temp dir.
fn resolve_work_dir(index_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(d) = index_dir {
        return Some(d.to_path_buf());
    }
    let tmp = std::env::temp_dir();
    if !tmp.as_os_str().is_empty() {
        Some(tmp)
    } else {
        None
    }
}

