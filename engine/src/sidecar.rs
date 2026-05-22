//! `.jsonidx` sidecar: a memory-mapped on-disk copy of the index. Saves
//! re-parsing on every open of the same file, and lets the OS page in
//! only the working-set records we actually touch.

use memmap2::Mmap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::document::NodeRecord;
use crate::error::EngineError;

/// Sidecar format version. Bumped on any change that alters the
/// on-disk layout *or* the parse semantics that produced it (so
/// behaviour-affecting parser fixes invalidate stale caches even
/// when the byte layout is unchanged). Older sidecars failing the
/// version check are silently reparsed.
pub const SIDECAR_VERSION: u32 = 12;
pub const SIDECAR_HEADER_SIZE: usize = 64;

/// Fixed-layout header at the start of every sidecar file.
///
/// `repr(C)` + 8-byte alignment + size = 64 bytes (multiple of records'
/// alignment) so the records section can begin at offset 64 with no
/// padding fix-up.
#[repr(C)]
pub struct SidecarHeader {
    pub magic: [u8; 4],         // "JIDX"
    pub version: u32,
    pub source_size: u64,
    pub source_mtime_ns: u64,
    pub node_count: u32,
    /// Diagnostic: count of fat-string records (length >= threshold).
    pub fat_string_count: u32,
    pub records_offset: u64,
    pub keys_offset: u64,
    /// Decoded keys arena size in bytes. `u64` because key-heavy >50 GB
    /// inputs produce arenas past 4 GiB; the previous `u32` truncated
    /// silently and the loaded keys section was short of the actual
    /// data, returning corrupt key bytes for late records.
    pub keys_size: u64,
    /// Diagnostic: largest `subtree_size` observed during parse. Useful
    /// for sanity checking on load and for sizing temporary buffers
    /// when materialising a subtree.
    pub max_subtree_size: u32,
    pub _reserved: u32,
}

const _: () = {
    assert!(size_of::<SidecarHeader>() == SIDECAR_HEADER_SIZE);
    assert!(align_of::<SidecarHeader>() == 8);
};

/// Computes the sidecar path inside `index_dir` for the given source.
/// Filename is `<fnv-hash>-<sanitized-stem>.jsonidx`.
pub fn sidecar_path(index_dir: &Path, source_path: &Path) -> PathBuf {
    let canonical = source_path.to_string_lossy();
    let h = fnv1a_hash(canonical.as_bytes());
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_stem)
        .unwrap_or_default();
    let name = if stem.is_empty() {
        format!("{:016x}.jsonidx", h)
    } else {
        format!("{:016x}-{}.jsonidx", h, stem)
    };
    index_dir.join(name)
}

fn sanitize_stem(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(32));
    for c in s.chars().take(32) {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        }
    }
    out
}

/// FNV-1a 64-bit hash. Deterministic across processes (unlike the std
/// DefaultHasher which is randomized for DoS resistance).
pub fn fnv1a_hash(s: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in s {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Writes `records` and `keys` to `path` atomically (temp + rename).
pub fn write_sidecar(
    path: &Path,
    source_size: u64,
    source_mtime_ns: u64,
    records: &[NodeRecord],
    keys: &[u8],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let temp_path = with_extension(path, "jsonidx.tmp");

    let records_offset = SIDECAR_HEADER_SIZE as u64;
    let records_bytes = records.len() * std::mem::size_of::<NodeRecord>();
    let keys_offset = records_offset + records_bytes as u64;

    // Compute diagnostic fields by scanning records once.
    let mut max_subtree_size: u32 = 0;
    let mut fat_string_count: u32 = 0;
    for r in records {
        if r.subtree_size > max_subtree_size {
            max_subtree_size = r.subtree_size;
        }
        if r.kind == crate::document::NodeKind::String as u8
            && r.length >= crate::document::FAT_STRING_THRESHOLD as u64
        {
            fat_string_count += 1;
        }
    }

    let header = SidecarHeader {
        magic: *b"JIDX",
        version: SIDECAR_VERSION,
        source_size,
        source_mtime_ns,
        node_count: records.len() as u32,
        keys_size: keys.len() as u64,
        records_offset,
        keys_offset,
        max_subtree_size,
        fat_string_count,
        _reserved: 0,
    };

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp_path)?;

    // Header
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            &header as *const SidecarHeader as *const u8,
            SIDECAR_HEADER_SIZE,
        )
    };
    file.write_all(header_bytes)?;

    // Records: 48 bytes each, with 6 bytes of trailing padding to keep
    // alignment after kind/flags.
    let records_slice = unsafe {
        std::slice::from_raw_parts(records.as_ptr() as *const u8, records_bytes)
    };
    file.write_all(records_slice)?;

    // Keys
    file.write_all(keys)?;

    file.sync_data()?;
    drop(file);

    std::fs::rename(&temp_path, path)?;
    Ok(())
}

fn with_extension(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

pub struct LoadedSidecar {
    pub mmap: Mmap,
    pub records_offset: usize,
    pub node_count: usize,
    pub keys_offset: usize,
    pub keys_size: usize,
}

pub fn try_load_sidecar(
    path: &Path,
    expected_size: u64,
    expected_mtime_ns: u64,
) -> Result<LoadedSidecar, EngineError> {
    let file = File::open(path).map_err(|e| EngineError::Io(e.to_string()))?;
    let metadata = file.metadata().map_err(|e| EngineError::Io(e.to_string()))?;
    if metadata.len() < SIDECAR_HEADER_SIZE as u64 {
        return Err(EngineError::Io("sidecar truncated (header)".into()));
    }
    let mmap = unsafe { Mmap::map(&file).map_err(|e| EngineError::Io(e.to_string()))? };

    if mmap.len() < SIDECAR_HEADER_SIZE {
        return Err(EngineError::Io("sidecar truncated".into()));
    }

    // SAFETY: mmap is page-aligned (well above 8-byte alignment) and the
    // bytes are at least SIDECAR_HEADER_SIZE long.
    let header = unsafe { &*(mmap.as_ptr() as *const SidecarHeader) };

    if header.magic != *b"JIDX" {
        return Err(EngineError::Io("sidecar: bad magic".into()));
    }
    if header.version != SIDECAR_VERSION {
        return Err(EngineError::Io("sidecar: version mismatch".into()));
    }
    if header.source_size != expected_size {
        return Err(EngineError::Io("sidecar: source size mismatch".into()));
    }
    if header.source_mtime_ns != expected_mtime_ns {
        return Err(EngineError::Io("sidecar: source mtime mismatch".into()));
    }

    let records_offset = header.records_offset as usize;
    let node_count = header.node_count as usize;
    let keys_offset = header.keys_offset as usize;
    let keys_size = header.keys_size as usize;

    // Records must start past the header. Without this, a corrupted
    // header with `records_offset = 0` would alias the records section
    // over the header itself, and `key_bytes()` for record id 0 would
    // hand back raw header bytes as a "key".
    if records_offset < SIDECAR_HEADER_SIZE {
        return Err(EngineError::Io("sidecar: records overlap header".into()));
    }
    if records_offset % std::mem::align_of::<NodeRecord>() != 0 {
        return Err(EngineError::Io("sidecar: records misaligned".into()));
    }
    let records_size = node_count
        .checked_mul(std::mem::size_of::<NodeRecord>())
        .ok_or_else(|| EngineError::Io("sidecar: records size overflow".into()))?;
    let records_end = records_offset
        .checked_add(records_size)
        .ok_or_else(|| EngineError::Io("sidecar: records end overflow".into()))?;
    if records_end > mmap.len() {
        return Err(EngineError::Io("sidecar: records beyond file".into()));
    }
    // Keys must start at or past the records section's end. A bit-flipped
    // (or hostile) sidecar with `keys_offset` mid-records would alias key
    // bytes onto record bytes — `key_bytes()` then reads raw `NodeRecord`
    // memory as a key string.
    if keys_offset < records_end {
        return Err(EngineError::Io("sidecar: keys overlap records".into()));
    }
    let keys_end = keys_offset
        .checked_add(keys_size)
        .ok_or_else(|| EngineError::Io("sidecar: keys end overflow".into()))?;
    if keys_end > mmap.len() {
        return Err(EngineError::Io("sidecar: keys beyond file".into()));
    }

    Ok(LoadedSidecar {
        mmap,
        records_offset,
        node_count,
        keys_offset,
        keys_size,
    })
}
