import Foundation

/// Per-context caps for value previews. Each case names the place a
/// preview string is rendered and pairs an optional source-byte ceiling
/// (used to bound the FFI read so a fat-string blob doesn't get
/// fully copied) with an optional display-character ceiling (used to
/// trim before the string goes into a Text view).
///
/// Bytes are checked against the FFI `valueStringPrefix(meta:maxBytes:)`
/// result; characters are checked against the resulting Swift String.
/// Either cap may fire independently — most callers want both.
enum PreviewBudget {
    /// Tree row leaf preview — full row width.
    case leafNode
    /// Single-line member-secondary text on an inspector row.
    case memberSecondary
    /// Value of one key/value pair inside an object preview rendered
    /// for an array element.
    case nestedValue
    /// Primitive array element rendered directly in an array preview.
    /// No character cap — single-line truncation happens at layout.
    case primitiveArrayElement

    var sourceBytes: UInt64? {
        switch self {
        case .leafNode: nil
        case .memberSecondary: 256
        case .nestedValue: 128
        case .primitiveArrayElement: 256
        }
    }

    var displayChars: Int? {
        switch self {
        case .leafNode: 80
        case .memberSecondary: 60
        case .nestedValue: 32
        case .primitiveArrayElement: nil
        }
    }
}

extension String {
    /// Trims to `cap` characters with a trailing `…` when over.
    /// Returns self unchanged when at or under the cap, or when `cap`
    /// is nil. `force` always trims regardless of length — use it when
    /// the source was already known to be byte-truncated upstream.
    func truncated(toChars cap: Int?, force: Bool = false) -> String {
        guard let cap else { return self }
        if !force && count <= cap { return self }
        return String(prefix(cap)) + "…"
    }
}
