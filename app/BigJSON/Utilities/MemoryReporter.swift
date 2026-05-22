import Darwin
import Foundation

nonisolated enum MemoryReporter {
    /// Process memory footprint in bytes — matches what Activity
    /// Monitor's "Memory" column shows. Reads `phys_footprint` from
    /// `TASK_VM_INFO`, which excludes file-backed mmap pages (the
    /// JSON file itself), shared library pages, and clean reusable
    /// memory. Using `mach_task_basic_info.resident_size` instead
    /// would over-report by gigabytes once the JSON file's pages
    /// have been faulted in. Returns 0 on failure.
    static func residentBytes() -> UInt64 {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size
        )
        let status = withUnsafeMutablePointer(to: &info) { ptr -> kern_return_t in
            ptr.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { rebound in
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), rebound, &count)
            }
        }
        return status == KERN_SUCCESS ? UInt64(info.phys_footprint) : 0
    }
}

/// Sliding-window CPU usage sampler. Each `samplePercent()` call
/// returns the share of CPU consumed since the previous call,
/// expressed the way Activity Monitor does — 100% means one core
/// fully busy, so a fully-pegged 8-core machine reports 800%.
/// State is per-instance (one shared instance lives in `.shared`),
/// guarded by a lock so SwiftUI's TimelineView can sample from any
/// actor without races.
nonisolated final class CPUReporter: @unchecked Sendable {
    static let shared = CPUReporter()

    private let lock = NSLock()
    private var lastCPUSeconds: Double
    private var lastWallTime: Date

    private init() {
        lastCPUSeconds = Self.processCPUSeconds()
        lastWallTime = Date()
    }

    /// CPU% since the previous sample. First call after init returns
    /// 0 (no window yet); subsequent calls return a non-negative
    /// number that can exceed 100 on multi-core workloads.
    func samplePercent() -> Double {
        let nowCPU = Self.processCPUSeconds()
        let nowWall = Date()
        lock.lock()
        defer { lock.unlock() }
        let cpuDelta = nowCPU - lastCPUSeconds
        let wallDelta = nowWall.timeIntervalSince(lastWallTime)
        lastCPUSeconds = nowCPU
        lastWallTime = nowWall
        guard wallDelta > 0.001 else { return 0 }
        return max(0, (cpuDelta / wallDelta) * 100.0)
    }

    /// Total user + system CPU time consumed by this process across
    /// all threads (including ones that have already exited), in
    /// seconds. `getrusage(RUSAGE_SELF)` is the canonical "whole
    /// process so far" reading on Darwin.
    private static func processCPUSeconds() -> Double {
        var usage = rusage()
        guard getrusage(RUSAGE_SELF, &usage) == 0 else { return 0 }
        let user = Double(usage.ru_utime.tv_sec)
            + Double(usage.ru_utime.tv_usec) / 1_000_000.0
        let system = Double(usage.ru_stime.tv_sec)
            + Double(usage.ru_stime.tv_usec) / 1_000_000.0
        return user + system
    }
}
