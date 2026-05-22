//! Loads a JSON file and prints parse-progress samples at fixed
//! intervals, plus the actual physical-memory footprint sampled from
//! the kernel via `proc_pid_rusage` (matches Activity Monitor's
//! "Memory" column). Anything longer than ~500 ms between forward
//! progress is flagged as a stall.
//!
//! Note: macOS `getrusage` and `/usr/bin/time -l` report
//! `maximum_resident_set_size` which counts file-backed mmap pages
//! whose physical residency is owned by the page cache (shared with
//! the kernel) — those don't actually pin RAM to this process. Use
//! `phys_footprint` from `proc_pid_rusage` instead for the number
//! that matches Activity Monitor.
//!
//! Usage: cargo run --release --example probe_load -- <path>

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::document::Document;
use engine::progress;

const POLL_MS: u64 = 100;
const STALL_THRESHOLD_MS: u128 = 500;

/// Returns this process's current `phys_footprint` in bytes. This is
/// what Activity Monitor reports under "Memory" — the actual unique
/// physical memory the kernel attributes to the process, excluding
/// shared file-backed pages held in the page cache.
fn current_phys_footprint() -> u64 {
    // SAFETY: bound by the kernel's documented contract — we pass our
    // own pid, ask for the V4 rusage struct, and read a u64 field that
    // the kernel always populates on Darwin 10.10+.
    #[repr(C)]
    #[derive(Default)]
    struct RusageInfoV4 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
        ri_cpu_time_qos_default: u64,
        ri_cpu_time_qos_maintenance: u64,
        ri_cpu_time_qos_background: u64,
        ri_cpu_time_qos_utility: u64,
        ri_cpu_time_qos_legacy: u64,
        ri_cpu_time_qos_user_initiated: u64,
        ri_cpu_time_qos_user_interactive: u64,
        ri_billed_system_time: u64,
        ri_serviced_system_time: u64,
        ri_logical_writes: u64,
        ri_lifetime_max_phys_footprint: u64,
        ri_instructions: u64,
        ri_cycles: u64,
        ri_billed_energy: u64,
        ri_serviced_energy: u64,
        ri_interval_max_phys_footprint: u64,
        ri_runnable_time: u64,
        ri_flags: u64,
    }
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV4) -> i32;
    }
    let mut info = RusageInfoV4::default();
    let pid = unsafe { libc::getpid() };
    let rc = unsafe { proc_pid_rusage(pid, 4 /* RUSAGE_INFO_V4 */, &mut info) };
    if rc != 0 {
        return 0;
    }
    info.ri_phys_footprint
}

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| panic!("usage: probe_load <path>"));
    let temp_idx = std::env::temp_dir().join("bigjson-probe-idx");
    let _ = std::fs::remove_dir_all(&temp_idx);
    std::fs::create_dir_all(&temp_idx).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    let probe_started = Instant::now();
    let probe = std::thread::spawn(move || {
        let mut last_parsed: u64 = 0;
        let mut last_change: Instant = Instant::now();
        let mut tick: u64 = 0;
        let mut max_stall_ms: u128 = 0;
        let mut max_phys_mb: f64 = 0.0;
        let mut stalls: Vec<(f64, u64, u128)> = Vec::new();
        loop {
            std::thread::sleep(Duration::from_millis(POLL_MS));
            if stop_clone.load(Ordering::Acquire) {
                break;
            }
            tick += 1;
            let (parsed, total) = progress::current_progress();
            let phys_mb = current_phys_footprint() as f64 / (1024.0 * 1024.0);
            if phys_mb > max_phys_mb {
                max_phys_mb = phys_mb;
            }
            let elapsed = probe_started.elapsed().as_secs_f64();
            if parsed != last_parsed {
                let since = last_change.elapsed().as_millis();
                if since >= STALL_THRESHOLD_MS {
                    stalls.push((elapsed, last_parsed, since));
                    eprintln!(
                        "  STALL: {:>5.2}s  resumed at parsed={:>5.2} GiB  paused {:>4} ms",
                        elapsed,
                        last_parsed as f64 / (1024.0 * 1024.0 * 1024.0),
                        since,
                    );
                    if since > max_stall_ms {
                        max_stall_ms = since;
                    }
                }
                last_parsed = parsed;
                last_change = Instant::now();
            }
            if tick % 10 == 0 {
                let pct = if total > 0 {
                    (parsed as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
                eprintln!(
                    "  t={:>5.2}s  parsed={:>5.2} GiB / {:>5.2} GiB  ({:>5.1}%)  phys={:>6.1} MiB",
                    elapsed,
                    parsed as f64 / (1024.0 * 1024.0 * 1024.0),
                    total as f64 / (1024.0 * 1024.0 * 1024.0),
                    pct,
                    phys_mb,
                );
            }
        }
        eprintln!();
        eprintln!("=== probe summary ===");
        eprintln!("  stalls:        {}", stalls.len());
        eprintln!("  max stall:     {} ms", max_stall_ms);
        eprintln!("  max phys mem:  {:.1} MiB  (Activity Monitor 'Memory' column)", max_phys_mb);
        if !stalls.is_empty() {
            eprintln!("  stall details:");
            for (t, parsed, ms) in &stalls {
                eprintln!(
                    "    @ t={:>5.2}s  parsed={:>5.2} GiB  paused {:>4} ms",
                    t,
                    *parsed as f64 / (1024.0 * 1024.0 * 1024.0),
                    ms,
                );
            }
        }
    });

    eprintln!("loading {} ...", path);
    let started = Instant::now();
    let result = Document::open(std::path::Path::new(&path), Some(&temp_idx));
    let elapsed = started.elapsed();
    stop.store(true, Ordering::Release);
    let _ = probe.join();

    match result {
        Ok(doc) => {
            eprintln!(
                "loaded {} records in {:.2}s",
                doc.records().len(),
                elapsed.as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("ERROR: {:?} (after {:.2}s)", e, elapsed.as_secs_f64());
            std::process::exit(1);
        }
    }
}
