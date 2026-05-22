import Foundation

nonisolated extension Engine.Document {
    /// Per-row metadata fetched in batches for the tree view.
    ///
    /// Under the hybrid emit-gate, `id == Engine.noNode` indicates a
    /// primitive child — it has no record. Use `keyString(meta:)` /
    /// `valueString(meta:)` to read its bytes; `engine_node_*` calls
    /// taking a `u32` id are not valid for primitive children.
    struct ChildMeta: Sendable {
        let id: UInt32
        let kind: UInt8
        let flags: UInt8
        let childCount: UInt32
        let keyOffset: UInt64
        let keyLength: UInt32
        /// Slot index for array elements (0 for object members).
        let arrayIndex: UInt32
        let valueOffset: UInt64
        let valueLength: UInt64

        var isArrayElement: Bool { (flags & 0x02) != 0 }
        var isObjectMember: Bool { (flags & 0x01) != 0 }
        /// True when `keyOffset` indexes the source mmap (raw bytes,
        /// between the JSON string's quotes). False when it indexes
        /// the document's decoded keys arena. Set for primitive
        /// object members; clear for record-bearing members.
        var keyInSource: Bool { (flags & 0x04) != 0 }
        /// True for primitive children (no record under the hybrid
        /// emit-gate).
        var isPrimitive: Bool { id == Engine.noNode }
    }

    /// Stateful iterator over a container's children. The constructor
    /// captures the parent and an initial scan state; `next(limit:)`
    /// fetches the next batch of up to `limit` entries in O(batch)
    /// time per call. Total cost across the full enumeration is
    /// O(source_bytes_in_parent), eliminating the quadratic
    /// re-scan-from-zero of the offset-based form.
    ///
    /// Use this for any paginated walk through a container — it's
    /// the difference between sub-second and hundreds-of-seconds on
    /// arrays with 100K+ children.
    final class ChildrenIterator: @unchecked Sendable {
        private let document: Engine.Document
        private let parent: UInt32
        private var state: EngineScanState
        private var done: Bool = false

        init(document: Engine.Document, parent: UInt32) {
            self.document = document
            self.parent = parent
            self.state = EngineScanState(
                // `UInt64.max` is the FFI's "uninitialised" sentinel;
                // 0 would collide with the legal cursor at the
                // parent's opening bracket.
                pos: UInt64.max,
                next_skippable: Engine.noNode,
                array_index: 0
            )
        }

        /// Returns the next up-to-`limit` children. Empty result
        /// means the iterator is exhausted; subsequent calls also
        /// return empty.
        func next(limit: Int) -> [ChildMeta] {
            guard !done, limit > 0 else { return [] }
            var buffer = [EngineChildMeta](
                repeating: EngineChildMeta(
                    id: 0, kind: 0, flags: 0, _pad: 0, child_count: 0,
                    key_offset: 0, key_length: 0, array_index: 0,
                    value_offset: 0, value_length: 0
                ),
                count: limit
            )
            let written = buffer.withUnsafeMutableBufferPointer { ptr -> Int in
                guard let base = ptr.baseAddress else { return 0 }
                return Int(engine_node_children_meta_batch_resume(
                    document.handle, parent, &state, UInt32(limit), base
                ))
            }
            if written == 0 { done = true; return [] }
            return (0..<written).map { ChildMeta(raw: buffer[$0]) }
        }
    }

    /// Constructs a resumable iterator over `parent`'s children.
    /// Preferred over `childrenMetaBatch(of:offset:limit:)` for any
    /// paginated walk — it amortises scan cost across calls.
    func childrenIterator(of parent: UInt32) -> ChildrenIterator {
        ChildrenIterator(document: self, parent: parent)
    }

    /// Fetches metadata for up to `limit` children of `parent` in one
    /// FFI call. **Note:** re-scans from the first child every call.
    /// For paginated walks of large containers, prefer
    /// `childrenIterator(of:)` — it's O(N) total vs O(N²) here.
    func childrenMetaBatch(of parent: UInt32, offset: Int = 0, limit: Int) -> [ChildMeta] {
        guard limit > 0 else { return [] }
        var buffer = [EngineChildMeta](
            repeating: EngineChildMeta(
                id: 0, kind: 0, flags: 0, _pad: 0, child_count: 0,
                key_offset: 0, key_length: 0, array_index: 0,
                value_offset: 0, value_length: 0
            ),
            count: limit
        )
        let written = buffer.withUnsafeMutableBufferPointer { ptr -> Int in
            guard let base = ptr.baseAddress else { return 0 }
            return Int(engine_node_children_meta_batch(
                handle, parent, UInt32(offset), UInt32(limit), base
            ))
        }
        return (0..<written).map { ChildMeta(raw: buffer[$0]) }
    }

    /// Reads a child's key bytes — either from the decoded keys
    /// arena (for record-bearing children) or directly from the
    /// source mmap (for primitives, which carry raw inter-quote
    /// spans). Source-keys are run through the engine's JSON
    /// string-escape decoder so the caller always gets a UTF-8
    /// String regardless of which arena the bytes came from.
    func keyString(meta: ChildMeta) -> String? {
        guard meta.isObjectMember, meta.keyLength > 0 else { return nil }
        let slice = engine_node_value_bytes_at(
            handle, meta.keyOffset, UInt64(meta.keyLength), meta.keyInSource ? 1 : 0
        )
        guard slice.length > 0, let data = slice.data else { return nil }
        if meta.keyInSource {
            let owned = engine_decode_json_string(data, slice.length)
            defer { engine_free_owned_bytes(owned) }
            guard let p = owned.data else { return nil }
            return String(
                decoding: UnsafeBufferPointer(start: p, count: Int(owned.length)),
                as: UTF8.self
            )
        }
        return String(
            decoding: UnsafeBufferPointer(start: data, count: Int(slice.length)),
            as: UTF8.self
        )
    }

    /// Reads the raw value bytes for a child. For containers and fat
    /// strings, this is the same source span as
    /// `engine_node_value_bytes` of `meta.id`; for primitives it's
    /// `valueOffset..valueOffset+valueLength` in the source mmap.
    func valueString(meta: ChildMeta) -> String? {
        guard meta.valueLength > 0 else { return nil }
        let slice = engine_node_value_bytes_at(
            handle, meta.valueOffset, meta.valueLength, /*sourceFlag=*/1
        )
        guard slice.length > 0, let data = slice.data else { return nil }
        return String(
            bytes: UnsafeBufferPointer(start: data, count: Int(slice.length)),
            encoding: .utf8
        )
    }

    /// Bounded variant of `valueString(meta:)` for previews: reads at
    /// most `maxBytes` source bytes and returns the lossy UTF-8 decode
    /// plus a `truncated` flag. Critical for fat-string rows in tree
    /// views — the unbounded form would copy the entire 500MB blob
    /// into a Swift `String` just to be sliced down to 60 chars. The
    /// bounded slice may cut mid-codepoint; `String(decoding:as:)`
    /// inserts a tail replacement char which is harmless in a
    /// "…"-truncated preview.
    func valueStringPrefix(meta: ChildMeta, maxBytes: UInt64) -> (text: String, truncated: Bool)? {
        guard meta.valueLength > 0 else { return nil }
        let n = min(meta.valueLength, maxBytes)
        let slice = engine_node_value_bytes_at(
            handle, meta.valueOffset, n, /*sourceFlag=*/1
        )
        guard slice.length > 0, let data = slice.data else { return nil }
        let buf = UnsafeBufferPointer(start: data, count: Int(slice.length))
        let text = String(decoding: buf, as: UTF8.self)
        return (text, n < meta.valueLength)
    }
}

private extension Engine.Document.ChildMeta {
    init(raw: EngineChildMeta) {
        self.init(
            id: raw.id,
            kind: raw.kind,
            flags: raw.flags,
            childCount: raw.child_count,
            keyOffset: raw.key_offset,
            keyLength: raw.key_length,
            arrayIndex: raw.array_index,
            valueOffset: raw.value_offset,
            valueLength: raw.value_length
        )
    }
}
