//! Parse-progress beacon. The parser advances a pair of process-wide
//! atomics holding `(parsed_bytes, total_bytes)`; UI polls them through
//! the FFI to drive a determinate progress bar.
//!
//! Process-wide rather than per-document because the UI only ever opens
//! one file at a time, and a global keeps the FFI surface signature-
//! free (no document handle to thread through while a load is in
//! flight). Concurrent opens would need a per-token registry layered on
//! top.

use std::sync::atomic::{AtomicU64, Ordering};

static PARSE_BYTES_PARSED: AtomicU64 = AtomicU64::new(0);
static PARSE_BYTES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Resets the beacon for a new parse. `total` is the source file size
/// so the UI can compute a fraction without an extra stat call.
pub fn reset_parse_progress(total: u64) {
    PARSE_BYTES_PARSED.store(0, Ordering::Relaxed);
    PARSE_BYTES_TOTAL.store(total, Ordering::Relaxed);
}

/// Reports the parser's current source-byte position. Called at
/// container-close boundaries (not on every byte) to keep atomic
/// contention negligible relative to the parser's work.
#[inline]
pub fn report_parse_progress(parsed: u64) {
    PARSE_BYTES_PARSED.store(parsed, Ordering::Relaxed);
}

/// Pins the bar at 100 % at end-of-parse so the UI's last poll sees a
/// clean completion frame regardless of where the parser's last
/// container-close report landed.
pub fn finish_parse_progress() {
    let total = PARSE_BYTES_TOTAL.load(Ordering::Relaxed);
    PARSE_BYTES_PARSED.store(total, Ordering::Relaxed);
}

/// UI-side reader. Returns `(parsed, total)`; both zero before the
/// first parse has started.
pub fn current_progress() -> (u64, u64) {
    (
        PARSE_BYTES_PARSED.load(Ordering::Relaxed),
        PARSE_BYTES_TOTAL.load(Ordering::Relaxed),
    )
}
