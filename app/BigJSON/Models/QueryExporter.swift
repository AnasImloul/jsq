import Foundation

/// Thin Swift wrapper around the engine's `engine_query_run_and_render`
/// FFI. The actual format logic — JSON encoding, CSV quoting, zero-copy
/// value lookups against the source mmap — lives in `engine::render`
/// on the Rust side. Both this exporter and the `jsq` CLI delegate to
/// the same code, so adding a new format or fixing an escape bug is a
/// one-place change.
nonisolated enum QueryExporter {
    enum Format {
        case jsonArray
        case ndjson
        case csv

        var defaultName: String {
            switch self {
            case .jsonArray: "results.json"
            case .ndjson:    "results.ndjson"
            case .csv:       "results.csv"
            }
        }

        /// Discriminant matching the Rust `ENGINE_RENDER_*` constants
        /// in `engine/src/ffi/query.rs`. Hardcoded here rather than
        /// imported because Rust `pub const` doesn't expose a stable
        /// C symbol — the values are the contract.
        fileprivate var ffiTag: UInt8 {
            switch self {
            case .ndjson:    0
            case .jsonArray: 1
            case .csv:       2
            }
        }
    }

    /// Re-runs `query` against the document and returns the rendered
    /// bytes. The re-run is intentional — the export menu fires long
    /// after the engine handle for the on-screen results has been
    /// freed, and the source is already in the OS page cache so the
    /// second pass is cheap.
    ///
    /// Returns nil on parse error or if the engine couldn't satisfy
    /// the query (e.g. missing index that auto-create can't supply).
    static func export(
        query: String,
        document: Engine.Document,
        format: Format,
        limit: Int = 5000
    ) -> Data? {
        let bytes = query.withCString { qPtr in
            engine_query_run_and_render(
                document.handle,
                qPtr,
                UInt32(limit),
                format.ffiTag
            )
        }
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else { return nil }
        return Data(bytes: data, count: Int(bytes.length))
    }
}
