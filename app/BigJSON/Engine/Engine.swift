import Foundation

/// Thin Swift wrapper around the Rust static-library engine.
///
/// `Engine` is the namespace; `Engine.Document` is the per-file
/// handle. Capability is split across a handful of sibling files so
/// each one stays focused on a single concern:
///
/// - `Engine.swift` — top-level surface (this file): namespace,
///   FFI-shared enums (`NodeKind`, `OpenError`), and the index cache
///   directory.
/// - `Engine+Grammar.swift` — grammar manifest, tokeniser, completion
///   context, query formatter, parse-progress polling.
/// - `Document.swift` — the `Document` class itself: handle ownership
///   and per-node accessors (kind / key / value / path / parent
///   chain).
/// - `Document+Children.swift` — `ChildMeta`, `ChildrenIterator`, and
///   batched children APIs.
/// - `Document+Query.swift` — query running, text search, foreign-key
///   index management, and the `QueryRun` / `IndexInfo` value types.
nonisolated enum Engine {
    static var version: String {
        guard let cstr = engine_version() else { return "unknown" }
        return String(cString: cstr)
    }

    /// Sentinel value returned by the engine to mean "no node".
    static let noNode: UInt32 = .max

    enum NodeKind: UInt8, Sendable {
        case null = 0
        case bool = 1
        case number = 2
        case string = 3
        case array = 4
        case object = 5

        func toJSONNodeType() -> JSONNodeType {
            switch self {
            case .null:   .null
            case .bool:   .bool
            case .number: .number
            case .string: .string
            case .array:  .array
            case .object: .object
            }
        }
    }

    enum OpenError: LocalizedError, Sendable {
        case failed(String)

        var errorDescription: String? {
            if case .failed(let m) = self { return m }
            return nil
        }
    }

    /// Index cache directory inside this app's container — always
    /// writable in the sandbox; survives across launches. Created on
    /// first use.
    static var indexCacheDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("BigJSON/IndexCache", isDirectory: true)
    }
}
