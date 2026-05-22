import Foundation

nonisolated struct QueryResult: Identifiable, Hashable, Sendable {
    let id: UUID
    let nodeID: UInt64?       // JSONNode-format id; nil for synthetic results
    let path: String
    let type: JSONNodeType
    let preview: String
    let fullText: String?

    init(
        id: UUID = UUID(),
        nodeID: UInt64?,
        path: String,
        type: JSONNodeType,
        preview: String,
        fullText: String?
    ) {
        self.id = id
        self.nodeID = nodeID
        self.path = path
        self.type = type
        self.preview = preview
        self.fullText = fullText
    }

}
