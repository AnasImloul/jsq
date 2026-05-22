import Foundation

nonisolated extension Engine {
    /// Owning wrapper around an opaque `EngineDocument *`. Closes the
    /// underlying handle in `deinit`. Safe to share across threads —
    /// the engine is read-only once parsed.
    ///
    /// Method coverage is split across sibling extensions:
    /// - `Document.swift` (this file): handle ownership and per-node
    ///   accessors keyed on a `u32` node id.
    /// - `Document+Children.swift`: `ChildMeta`, `ChildrenIterator`,
    ///   and batched children APIs.
    /// - `Document+Query.swift`: query running, text search, and
    ///   foreign-key index management.
    final class Document: @unchecked Sendable {
        /// Engine-owned pointer; treat as `private` to the Document
        /// type's own files. Exposed at module scope so the sibling
        /// extensions can call FFI on it.
        let handle: OpaquePointer
        let url: URL
        let fileSize: Int64
        let totalNodeCount: Int
        let rootID: UInt32
        let idScope: UInt64
        let loadedFromSidecar: Bool

        private static let scopeLock = NSLock()
        private static var scopeCounter: UInt64 = 1

        init(opening url: URL) throws {
            let didStart = url.startAccessingSecurityScopedResource()
            defer { if didStart { url.stopAccessingSecurityScopedResource() } }

            let cacheDir = Engine.indexCacheDirectory
            try? FileManager.default.createDirectory(at: cacheDir, withIntermediateDirectories: true)

            let sourcePath = url.path(percentEncoded: false)
            let cachePath = cacheDir.path(percentEncoded: false)

            let raw: OpaquePointer? = sourcePath.withCString { srcPtr in
                cachePath.withCString { dirPtr in
                    engine_open(srcPtr, dirPtr)
                }
            }
            guard let h = raw else {
                let msg = engine_last_error().map { String(cString: $0) } ?? "unknown error"
                throw OpenError.failed(msg)
            }
            self.handle = h
            self.url = url
            self.fileSize = Int64(engine_file_size(h))
            self.totalNodeCount = Int(engine_total_node_count(h))
            self.rootID = engine_root(h)
            self.loadedFromSidecar = engine_loaded_from_sidecar(h) != 0

            Document.scopeLock.lock()
            self.idScope = Document.scopeCounter
            Document.scopeCounter &+= 1
            Document.scopeLock.unlock()
        }

        deinit {
            engine_close(handle)
        }

        // MARK: Node queries

        func kind(of node: UInt32) -> NodeKind {
            NodeKind(rawValue: engine_node_kind(handle, node)) ?? .null
        }

        func parent(of node: UInt32) -> UInt32? {
            let p = engine_node_parent(handle, node)
            return p == Engine.noNode ? nil : p
        }

        func firstChild(of node: UInt32) -> UInt32? {
            let c = engine_node_first_child(handle, node)
            return c == Engine.noNode ? nil : c
        }

        func nextSibling(of node: UInt32) -> UInt32? {
            let s = engine_node_next_sibling(handle, node)
            return s == Engine.noNode ? nil : s
        }

        func childCount(of node: UInt32) -> Int {
            Int(engine_node_child_count(handle, node))
        }

        /// Per-kind counts across *all* of `node`'s children. Returned
        /// as a `[JSONNodeType: Int]` so callers don't need to know the
        /// engine's raw indices. Walks the linked list once on the
        /// engine side; cheap even for million-element arrays.
        func childrenKindCounts(of node: UInt32) -> [JSONNodeType: Int] {
            var raw = [UInt32](repeating: 0, count: 6)
            raw.withUnsafeMutableBufferPointer { buf in
                _ = engine_node_children_kind_counts(handle, node, buf.baseAddress)
            }
            var out: [JSONNodeType: Int] = [:]
            // Indices match the engine's NodeKind enum: 0=null, 1=bool,
            // 2=number, 3=string, 4=array, 5=object.
            let order: [JSONNodeType] = [.null, .bool, .number, .string, .array, .object]
            for (i, type) in order.enumerated() where raw[i] > 0 {
                out[type] = Int(raw[i])
            }
            return out
        }

        /// Byte offset of the node's value in the source file.
        func byteOffset(of node: UInt32) -> UInt64 {
            engine_node_byte_offset(handle, node)
        }

        /// Byte length of the node's value in the source file. `UInt64`
        /// because the root container of a multi-GB document overflows
        /// 32 bits.
        func byteLength(of node: UInt32) -> UInt64 {
            engine_node_byte_length(handle, node)
        }

        func valueString(of node: UInt32) -> String? {
            let slice = engine_node_value_bytes(handle, node)
            guard slice.length > 0, let data = slice.data else { return nil }
            return String(bytes: UnsafeBufferPointer(start: data, count: Int(slice.length)),
                          encoding: .utf8)
        }

        func keyString(of node: UInt32) -> String? {
            let slice = engine_node_key(handle, node)
            guard slice.length > 0, let data = slice.data else { return nil }
            return String(bytes: UnsafeBufferPointer(start: data, count: Int(slice.length)),
                          encoding: .utf8)
        }

        func arrayIndex(of node: UInt32) -> Int? {
            if engine_node_is_array_element(handle, node) != 0 {
                return Int(engine_node_array_index(handle, node))
            }
            return nil
        }

        func path(of node: UInt32) -> String {
            let owned = engine_node_path(handle, node)
            defer { engine_free_owned_bytes(owned) }
            guard owned.length > 0, let data = owned.data else { return "." }
            return String(bytes: UnsafeBufferPointer(start: data, count: Int(owned.length)),
                          encoding: .utf8) ?? "."
        }

        // MARK: Composite helpers

        /// Builds the `NodeKey` for a node by inspecting whether it's an
        /// object member, array element, or root.
        func nodeKey(of node: UInt32) -> NodeKey {
            if let key = keyString(of: node) {
                return .key(key)
            }
            if let idx = arrayIndex(of: node) {
                return .index(idx)
            }
            return .root
        }

        /// Walks the engine's first-child + next-sibling chain.
        /// Capped iteration is the caller's responsibility.
        struct ChildIDSequence: Sequence, IteratorProtocol {
            let document: Document
            var current: UInt32?

            mutating func next() -> UInt32? {
                guard let id = current else { return nil }
                current = document.nextSibling(of: id)
                return id
            }
        }

        func childIDs(of node: UInt32) -> ChildIDSequence {
            ChildIDSequence(document: self, current: firstChild(of: node))
        }

        /// Fetches up to `limit` child IDs in a single FFI call.
        /// Avoids the per-sibling FFI overhead of `childIDs(of:)` when
        /// we know we want a bounded prefix (the common case for tree
        /// expansion).
        func childIDsBatch(of node: UInt32, offset: Int = 0, limit: Int) -> [UInt32] {
            guard limit > 0 else { return [] }
            var buffer = [UInt32](repeating: 0, count: limit)
            let written = buffer.withUnsafeMutableBufferPointer { ptr -> Int in
                guard let base = ptr.baseAddress else { return 0 }
                return Int(engine_node_children_batch(
                    handle, node, UInt32(offset), UInt32(limit), base
                ))
            }
            if written < limit {
                buffer.removeLast(limit - written)
            }
            return buffer
        }

        /// Returns the JSONNode-format ID for a node in this document.
        func jsonNodeID(for node: UInt32) -> UInt64 {
            (idScope << 32) | UInt64(node)
        }

        /// Recovers an engine node ID from a JSONNode-format ID,
        /// returning nil if the ID belongs to a different document or
        /// is a synthetic.
        func engineNodeID(from jsonNodeID: UInt64) -> UInt32? {
            let scope = jsonNodeID >> 32
            if scope != idScope { return nil }
            let nodeID = UInt32(jsonNodeID & 0xFFFFFFFF)
            if Int(nodeID) >= totalNodeCount { return nil }
            return nodeID
        }

        /// View-model wrapper for the document's root, ready for the
        /// tree view.
        var rootNode: JSONNode {
            JSONNode.fromEngine(document: self, nodeID: rootID)
        }

        /// Looks up a `JSONNode` by its public JSONNode-format id, or
        /// returns nil when the id belongs to another document or
        /// references a synthetic.
        func node(for jsonNodeID: UInt64) -> JSONNode? {
            guard let nodeID = engineNodeID(from: jsonNodeID) else { return nil }
            return JSONNode.fromEngine(document: self, nodeID: nodeID)
        }
    }
}
