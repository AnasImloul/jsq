import SwiftUI

/// Clickable path. Each segment is a button that sets `selection` to the
/// corresponding ancestor; the trailing segment (the current node) is
/// plain text.
struct BreadcrumbView: View {
    let node: JSONNode
    let document: Engine.Document
    @Binding var selection: JSONNode.ID?

    /// Cached chain — recomputed only when `node.id` changes. Building
    /// the chain walks the parent pointers and calls `engine.path(of:)`
    /// per ancestor (which itself walks to root), so it's O(depth²)
    /// FFI calls. Doing that work inside `body` re-runs it on every
    /// invalidation; gate it behind a `.task(id:)` instead.
    @State private var chain: [Entry] = []

    var body: some View {
        let lastIndex = chain.count - 1
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(0 ..< chain.count, id: \.self) { (i: Int) in
                    segmentRow(entry: chain[i], isLast: i == lastIndex)
                }
            }
        }
        .task(id: node.id) {
            chain = computeAncestorChain()
        }
    }

    @ViewBuilder
    private func segmentRow(entry: Entry, isLast: Bool) -> some View {
        if entry.position > 0 {
            Image(systemName: "chevron.right")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(.tertiary)
        }
        if isLast {
            Text(entry.label)
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(.primary)
        } else {
            Button {
                selection = entry.nodeJSONID
            } label: {
                Text(entry.label)
                    .font(.system(.callout, design: .monospaced))
                    .foregroundStyle(Color.accentColor)
            }
            .buttonStyle(.plain)
            .help("Go to \(entry.fullPath)")
        }
    }

    private struct Entry: Identifiable {
        let position: Int
        let nodeJSONID: UInt64
        let label: String
        let fullPath: String
        var id: Int { position }
    }

    /// Walks the engine's parent chain from the current node to the root,
    /// returning entries in root → leaf order. Called only when `node.id`
    /// changes (via `.task(id:)`), not on every redraw.
    private func computeAncestorChain() -> [Entry] {
        guard let leafEngineID = document.engineNodeID(from: node.id) else {
            return []
        }
        var ids: [UInt32] = [leafEngineID]
        var cur: UInt32 = leafEngineID
        while let parent = document.parent(of: cur) {
            ids.append(parent)
            cur = parent
        }
        return ids.reversed().enumerated().map { offset, engineID in
            Entry(
                position: offset,
                nodeJSONID: document.jsonNodeID(for: engineID),
                label: label(for: engineID),
                fullPath: document.path(of: engineID)
            )
        }
    }

    private func label(for engineID: UInt32) -> String {
        if document.parent(of: engineID) == nil {
            return "$"
        }
        if let key = document.keyString(of: engineID), !key.isEmpty {
            return key
        }
        if let idx = document.arrayIndex(of: engineID) {
            return "[\(idx)]"
        }
        return "?"
    }
}
