import Foundation

nonisolated extension Engine {
    struct QueryParseError: LocalizedError, Sendable {
        let message: String
        let position: Int
        var errorDescription: String? { message }
    }
}

nonisolated extension Engine.Document {
    struct QueryRun: Sendable {
        let results: [QueryResult]
        let hitLimit: Bool
        /// Set when evaluation hit a `lookup(SOURCE; KEY)` with no
        /// matching index in the registry. The UI surfaces this as a
        /// banner with a "Create index" button. `results` is empty in
        /// this case — partial results are not propagated.
        let missingIndex: MissingIndex?
        /// How many rows the source path emitted before the rest of
        /// the pipeline (filter / aggregate / sort) ran. Surfaced in
        /// the stats popover so users can see how much data the query
        /// actually walked.
        let scannedRows: UInt64
        /// Successful `lookup(...)` invocations during this query.
        /// Surfaced in the popover so users can spot field-set fanout
        /// at a glance.
        let lookupCalls: UInt64
        /// Sum of source byte spans for every node the source path
        /// emitted. Compare against the document's file size to spot
        /// memory-bandwidth-bound queries — when this approaches the
        /// file size the engine touched most of the document.
        let scannedBytes: UInt64
    }

    struct MissingIndex: Sendable, Equatable {
        let source: String
        let key: String
    }

    struct IndexInfo: Sendable, Identifiable {
        let source: String
        let key: String
        let sourceCount: Int
        let indexedCount: Int
        let bucketCount: Int
        let approxBytes: Int

        var id: String { "\(source)\u{1F}\(key)" }
    }

    struct IndexBuildStats: Sendable {
        let sourceCount: Int
        let indexedCount: Int
        let bucketCount: Int
        let approxBytes: Int
    }

    /// Returns the set of node kinds produced by sampling up to
    /// `limit` outputs of `query`. Empty on parse error / no outputs.
    /// Used by autocomplete to switch between key and array-accessor
    /// suggestion modes.
    func kindsForQuery(_ query: String, limit: Int = 5000) -> Set<JSONNodeType> {
        let bitmask = query.withCString { engine_kinds_for_query(handle, $0, UInt32(limit)) }
        var result: Set<JSONNodeType> = []
        if bitmask & (1 << 0) != 0 { result.insert(.null) }
        if bitmask & (1 << 1) != 0 { result.insert(.bool) }
        if bitmask & (1 << 2) != 0 { result.insert(.number) }
        if bitmask & (1 << 3) != 0 { result.insert(.string) }
        if bitmask & (1 << 4) != 0 { result.insert(.array) }
        if bitmask & (1 << 5) != 0 { result.insert(.object) }
        return result
    }

    /// Returns the union of object keys produced by running `query`,
    /// sampled up to `limit` outputs. Used for schema-aware
    /// autocomplete. Returns [] on parse error or when the query
    /// doesn't yield any object outputs.
    func keysForQuery(_ query: String, limit: Int = 5000) -> [String] {
        let bytes = query.withCString { engine_keys_for_query(handle, $0, UInt32(limit)) }
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else { return [] }
        let jsonData = Data(bytes: data, count: Int(bytes.length))
        return (try? JSONSerialization.jsonObject(with: jsonData) as? [String]) ?? []
    }

    func runQuery(_ text: String, limit: Int = 5000) -> Result<QueryRun, Engine.QueryParseError> {
        let raw: OpaquePointer? = text.withCString { cstr in
            engine_query_run(handle, cstr, UInt32(limit))
        }
        guard let h = raw else {
            let msg = engine_query_last_parse_error().map { String(cString: $0) }
                ?? "parse error"
            let pos = Int(engine_query_last_parse_error_position())
            return .failure(Engine.QueryParseError(message: msg, position: pos))
        }
        defer { engine_query_results_free(h) }
        return .success(extractQueryRun(from: h))
    }

    /// Plain-text substring search across the whole document. Used
    /// when the query bar input doesn't parse as jq — the user is
    /// looking for "any node whose key or value contains this".
    func runTextSearch(_ needle: String, limit: Int = 5000) -> QueryRun {
        let raw: OpaquePointer? = needle.withCString { cstr in
            engine_query_text_search(handle, cstr, UInt32(limit))
        }
        guard let h = raw else {
            return QueryRun(
                results: [],
                hitLimit: false,
                missingIndex: nil,
                scannedRows: 0,
                lookupCalls: 0,
                scannedBytes: 0
            )
        }
        defer { engine_query_results_free(h) }
        return extractQueryRun(from: h)
    }

    private func extractQueryRun(from h: OpaquePointer) -> QueryRun {
        // Missing-index check first — the engine returns an empty
        // result set in this case, so we don't waste a copy loop.
        let missing: MissingIndex? = {
            guard let srcPtr = engine_query_results_missing_index_source(h),
                  let keyPtr = engine_query_results_missing_index_key(h) else {
                return nil
            }
            return MissingIndex(
                source: String(cString: srcPtr),
                key: String(cString: keyPtr)
            )
        }()
        let count = Int(engine_query_results_count(h))
        let hitLimit = engine_query_results_hit_limit(h) != 0
        var results: [QueryResult] = []
        results.reserveCapacity(count)
        for i in 0..<count {
            let view = engine_query_results_at(h, UInt32(i))
            let path = sliceToString(view.path)
            let preview = sliceToString(view.preview)
            let fullText = sliceToString(view.full_text)
            let nodeID: UInt64? = view.node_id == Engine.noNode
                ? nil
                : jsonNodeID(for: view.node_id)
            let type = Engine.NodeKind(rawValue: view.kind)?.toJSONNodeType() ?? .null
            results.append(QueryResult(
                nodeID: nodeID,
                path: path,
                type: type,
                preview: preview,
                fullText: fullText.isEmpty ? nil : fullText
            ))
        }
        return QueryRun(
            results: results,
            hitLimit: hitLimit,
            missingIndex: missing,
            scannedRows: engine_query_results_scanned_rows(h),
            lookupCalls: engine_query_results_lookup_calls(h),
            scannedBytes: engine_query_results_scanned_bytes(h)
        )
    }

    // MARK: Foreign-key indexes

    /// Builds and registers a foreign-key index on `(source, key)`.
    /// Returns nil on parse error.
    @discardableResult
    func createIndex(source: String, key: String) -> IndexBuildStats? {
        let stats = source.withCString { srcPtr in
            key.withCString { keyPtr in
                engine_query_create_index(handle, srcPtr, keyPtr)
            }
        }
        guard stats.ok != 0 else { return nil }
        return IndexBuildStats(
            sourceCount: Int(stats.source_count),
            indexedCount: Int(stats.indexed_count),
            bucketCount: Int(stats.bucket_count),
            approxBytes: Int(stats.approx_bytes)
        )
    }

    @discardableResult
    func dropIndex(source: String, key: String) -> Bool {
        let dropped = source.withCString { srcPtr in
            key.withCString { keyPtr in
                engine_query_drop_index(handle, srcPtr, keyPtr)
            }
        }
        return dropped != 0
    }

    func listIndexes() -> [IndexInfo] {
        let bytes = engine_query_list_indexes(handle)
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else { return [] }
        let jsonData = Data(bytes: data, count: Int(bytes.length))
        guard let arr = try? JSONSerialization.jsonObject(with: jsonData) as? [[String: Any]] else {
            return []
        }
        return arr.compactMap { entry -> IndexInfo? in
            guard let source = entry["source"] as? String,
                  let key = entry["key"] as? String else { return nil }
            return IndexInfo(
                source: source,
                key: key,
                sourceCount: (entry["source_count"] as? Int) ?? 0,
                indexedCount: (entry["indexed_count"] as? Int) ?? 0,
                bucketCount: (entry["bucket_count"] as? Int) ?? 0,
                approxBytes: (entry["approx_bytes"] as? Int) ?? 0
            )
        }
    }

    private func sliceToString(_ slice: EngineSlice) -> String {
        guard slice.length > 0, let data = slice.data else { return "" }
        return String(bytes: UnsafeBufferPointer(start: data, count: Int(slice.length)),
                      encoding: .utf8) ?? ""
    }
}
