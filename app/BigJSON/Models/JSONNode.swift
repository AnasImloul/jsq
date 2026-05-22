import Foundation

nonisolated enum JSONNodeType: Hashable, Sendable {
    case object, array, string, number, bool, null

    var badge: String {
        switch self {
        case .object: "{}"
        case .array: "[]"
        case .string: "abc"
        case .number: "123"
        case .bool: "T/F"
        case .null: "null"
        }
    }

    var label: String {
        switch self {
        case .object: "Object"
        case .array: "Array"
        case .string: "String"
        case .number: "Number"
        case .bool: "Boolean"
        case .null: "Null"
        }
    }

    var isContainer: Bool {
        self == .object || self == .array
    }
}

nonisolated enum NodeKey: Hashable, Sendable {
    case root
    case key(String)
    case index(Int)

    var display: String {
        switch self {
        case .root: "$"
        case .key(let s): s
        case .index(let i): "[\(i)]"
        }
    }
}

/// View-model wrapper around a `(Engine.Document, nodeID)` pair.
///
/// `path` and `leafFullText` are computed via FFI on first access and
/// cached, so SwiftUI's repeated property reads (during expansion,
/// layout, and diffing) don't re-cross the boundary. Children
/// enumeration goes through `Engine.Document.childrenIterator`, not
/// through this type — the iterator paginates lazily and avoids the
/// O(N²) re-scan that an eager `children` property would force.
///
/// Primitive children under the hybrid emit-gate aren't represented
/// as `JSONNode`s — they're surfaced as `Engine.Document.ChildMeta`
/// rows directly by the inspector container view.
nonisolated final class JSONNode: Identifiable, Hashable, @unchecked Sendable {
    let id: UInt64
    let key: NodeKey
    let type: JSONNodeType
    let leafPreview: String?

    let document: Engine.Document
    let nodeID: UInt32

    // Lazy-cached accessors. Mutated only via `cacheLock`. Tree views
    // read from the main actor; the lock is here as defense-in-depth
    // and is negligible-cost on the uncontended path.
    private let cacheLock = NSLock()
    private var _path: String?
    private var _leafFullText: String??

    init(
        id: UInt64,
        key: NodeKey,
        type: JSONNodeType,
        leafPreview: String?,
        document: Engine.Document,
        nodeID: UInt32
    ) {
        self.id = id
        self.key = key
        self.type = type
        self.leafPreview = leafPreview
        self.document = document
        self.nodeID = nodeID
    }

    var path: String {
        cacheLock.lock(); defer { cacheLock.unlock() }
        if let cached = _path { return cached }
        let computed = document.path(of: nodeID)
        _path = computed
        return computed
    }

    var leafFullText: String? {
        cacheLock.lock(); defer { cacheLock.unlock() }
        if let cached = _leafFullText { return cached }
        let computed: String? = type.isContainer ? nil : document.valueString(of: nodeID)
        _leafFullText = .some(computed)
        return computed
    }

    var childCount: Int {
        document.childCount(of: nodeID)
    }

    /// Source-byte length of this node's value (including any
    /// surrounding JSON syntax — e.g., quotes for strings, brackets for
    /// containers). O(1) — single FFI call. Used by header summaries
    /// that need a size badge without loading the value.
    var byteLength: UInt64 {
        document.byteLength(of: nodeID)
    }

    static func == (lhs: JSONNode, rhs: JSONNode) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

nonisolated extension JSONNode {
    /// Constructs a JSONNode. Eagerly fetches just enough to render
    /// the row (kind, key, leafPreview); path / fullText / children
    /// stay lazy and are cached on first access.
    static func fromEngine(document: Engine.Document, nodeID: UInt32) -> JSONNode {
        let kind = document.kind(of: nodeID).toJSONNodeType()
        let key = document.nodeKey(of: nodeID)
        let id = document.jsonNodeID(for: nodeID)

        let leafPreview: String?
        if kind.isContainer {
            leafPreview = nil
        } else {
            let raw = document.valueString(of: nodeID) ?? ""
            leafPreview = raw.truncated(toChars: PreviewBudget.leafNode.displayChars)
        }

        return JSONNode(
            id: id,
            key: key,
            type: kind,
            leafPreview: leafPreview,
            document: document,
            nodeID: nodeID
        )
    }
}
