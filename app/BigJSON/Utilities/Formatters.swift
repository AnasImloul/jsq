import Foundation

nonisolated enum Formatters {
    static func bytes(_ count: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: count, countStyle: .file)
    }

    static func bytes(_ count: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(min(count, UInt64(Int64.max))), countStyle: .memory)
    }

    static func count(_ n: Int) -> String {
        NumberFormatter.localizedString(from: NSNumber(value: n), number: .decimal)
    }

    /// Human-friendly elapsed-time label used by the query results
    /// header. Picks a unit that avoids both "0 ms" for sub-millisecond
    /// queries and noisy decimals for slow ones.
    static func duration(_ seconds: TimeInterval) -> String {
        let s = max(0, seconds)
        if s < 0.001 {
            // < 1 ms — render in microseconds, integer.
            return "\(Int((s * 1_000_000).rounded())) µs"
        }
        if s < 1.0 {
            // 1 ms .. 999 ms — integer ms, since the surrounding
            // header is already terse.
            return "\(Int((s * 1_000).rounded())) ms"
        }
        // ≥ 1 s — two-significant-figures-ish. "1.23 s", "12.3 s".
        if s < 10.0 {
            return String(format: "%.2f s", s)
        }
        return String(format: "%.1f s", s)
    }

    /// CPU% display matching Activity Monitor: integer for the
    /// idle/normal range, one decimal once we're under 10% so a
    /// quiet app doesn't flicker between "0%" and "1%". Above 100%
    /// we drop the decimal entirely — multi-core busy values are
    /// noisy by nature, and a single digit reads cleaner.
    static func cpuPercent(_ percent: Double) -> String {
        let p = max(0, percent)
        if p < 10 {
            return String(format: "%.1f%%", p)
        }
        return "\(Int(p.rounded()))%"
    }
}
