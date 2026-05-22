import SwiftUI

struct InspectorContainerSection: View {
    let node: JSONNode
    let document: Engine.Document
    @Binding var selection: JSONNode.ID?

    /// Loaded prefix of the container's children. Grows as the user
    /// scrolls past the load-more stub. For an unbounded array we never
    /// fetch beyond what's visible + a few pages ahead.
    @State private var metas: [Engine.Document.ChildMeta] = []
    @State private var loadedCount: Int = 0
    /// Resumable iterator — paginating with this is O(N) total instead
    /// of O(N²) for the offset-based form. Critical for arrays in the
    /// hundreds-of-thousands of children.
    @State private var iterator: Engine.Document.ChildrenIterator?
    /// Reference-typed preview cache — survives row recycling and does
    /// not trigger SwiftUI invalidation when mutated, so re-entering a
    /// row from below skips the FFI work.
    @State private var previewCache = ChildPreviewCache()

    /// Selection only ever pulls one page from the resumable iterator;
    /// the scroll-driven stub picks up subsequent chunks. No
    /// whole-container preload (histograms / kind counts) — those would
    /// force a full child walk and turn a 10M-item selection into a
    /// multi-second freeze.
    private static let initialWindow = 500
    private static let pageSize = 500

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Inspector.sectionLabel("Children — \(Formatters.count(node.childCount))")
            childListSection
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .task(id: node.id) {
            metas = []
            loadedCount = 0
            iterator = document.childrenIterator(of: engineID)
            previewCache.reset()
            loadFirstPage()
        }
    }

    @ViewBuilder
    private var childListSection: some View {
        if node.childCount == 0 {
            Text("Empty")
                .font(.callout)
                .foregroundStyle(.secondary)
        } else if metas.isEmpty {
            Text("Loading…")
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            LazyVStack(alignment: .leading, spacing: 1) {
                ForEach(Array(metas.enumerated()), id: \.offset) { idx, meta in
                    let childJSONID: JSONNode.ID? = meta.isPrimitive
                        ? nil
                        : document.jsonNodeID(for: meta.id)
                    InspectorChildRow(
                        meta: meta,
                        rowKey: idx,
                        document: document,
                        cache: previewCache,
                        isSelected: childJSONID != nil && childJSONID == selection,
                        onTap: {
                            // Primitives don't have engine record IDs and
                            // therefore can't be selected as the inspector's
                            // focus yet — clicking is a no-op. Following
                            // commit will introduce primitive handles.
                            if let id = childJSONID { selection = id }
                        }
                    )
                }
                if loadedCount < node.childCount {
                    LoadMoreStub(
                        loadedCount: loadedCount,
                        totalCount: node.childCount,
                        pageSize: Self.pageSize,
                        onAppear: { loadNextPage() }
                    )
                    // Force fresh @State (didTrigger) on every page boundary
                    // so the stub fires once per page, not once forever.
                    .id(loadedCount)
                }
            }
        }
    }

    private var engineID: UInt32 {
        document.engineNodeID(from: node.id) ?? 0
    }

    private func loadFirstPage() {
        let total = node.childCount
        if total == 0 { return }
        guard let it = iterator else { return }
        let target = min(Self.initialWindow, total)
        let fetched = it.next(limit: target)
        metas = fetched
        loadedCount = fetched.count
    }

    private func loadNextPage() {
        let total = node.childCount
        if loadedCount >= total { return }
        guard let it = iterator else { return }
        let toLoad = min(Self.pageSize, total - loadedCount)
        let fetched = it.next(limit: toLoad)
        if fetched.isEmpty { return }
        metas.append(contentsOf: fetched)
        loadedCount += fetched.count
    }
}

/// One-shot row that fires `onAppear` the first time it scrolls into
/// view, asking the parent to load the next page. The parent assigns a
/// fresh `.id(loadedCount)` so each page boundary gets a new stub
/// instance with reset `@State`.
struct LoadMoreStub: View {
    let loadedCount: Int
    let totalCount: Int
    let pageSize: Int
    let onAppear: () -> Void

    @State private var didTrigger = false

    var body: some View {
        HStack(spacing: 6) {
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.6)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .onAppear {
            guard !didTrigger else { return }
            didTrigger = true
            onAppear()
        }
    }

    private var label: String {
        let remaining = totalCount - loadedCount
        let next = min(pageSize, remaining)
        return "Loading next \(Formatters.count(next)) of \(Formatters.count(remaining)) more…"
    }
}

/// Reference-typed bag of array-element previews. Keyed by row index
/// rather than engine node id because under the hybrid emit-gate
/// primitive children all share `id == NULL_NODE` — the row index is
/// the only stable identifier for in-list addressing. Held by
/// `InspectorContainerSection` as `@State` and passed by reference to
/// each row; mutating it does NOT invalidate SwiftUI views (it's a
/// class with no observation), so a row recycling back into view can
/// read its previously computed preview for free instead of re-running
/// FFI.
///
/// FIFO-bounded so paging through a 10M-element array doesn't pin
/// gigabytes of preview entries on the heap. Each
/// `InspectorChildRow.ArrayPreview.object` can carry up to 12
/// key/value PreviewParts (~hundreds of bytes); we cap at `capacity`
/// total entries and evict the oldest insertion when the cap is
/// exceeded.
final class ChildPreviewCache {
    /// Tuned for "viewport + a few pages of overscroll" — bigger than
    /// any realistic scroll velocity needs to round-trip without
    /// re-firing FFI, small enough that worst-case heap is bounded
    /// (~1–2 MB for InspectorChildRow.ArrayPreview entries).
    private static let capacity = 2048

    private(set) var entries: [Int: InspectorChildRow.ArrayPreview] = [:]
    /// Insertion-order keys; the head is the oldest. Only updated for
    /// fresh keys — re-stores of an existing key keep their original
    /// position to avoid an O(N) `firstIndex` lookup on every set.
    private var order: [Int] = []

    func get(_ key: Int) -> InspectorChildRow.ArrayPreview? { entries[key] }

    func set(_ key: Int, _ value: InspectorChildRow.ArrayPreview) {
        if entries[key] == nil {
            order.append(key)
            if entries.count >= Self.capacity {
                let evict = order.removeFirst()
                entries.removeValue(forKey: evict)
            }
        }
        entries[key] = value
    }

    func reset() {
        entries.removeAll(keepingCapacity: true)
        order.removeAll(keepingCapacity: true)
    }
}
