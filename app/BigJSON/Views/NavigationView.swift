import SwiftUI

/// Single-column inspector that drives the user's primary navigation:
/// drill in by clicking children, drill up by clicking ancestor
/// breadcrumb segments. Section views (header, path, value, container,
/// metadata) live under `Views/Inspector/`.
struct NavigationView: View {
    let document: Engine.Document
    @Binding var selection: JSONNode.ID?

    var body: some View {
        ScrollView {
            Group {
                if let id = selection, let node = document.node(for: id) {
                    VStack(alignment: .leading, spacing: 0) {
                        InspectorHeader(node: node)
                        Divider().padding(.horizontal, 16)
                        InspectorPathSection(
                            node: node,
                            document: document,
                            selection: $selection
                        )
                        Divider().padding(.horizontal, 16)
                        if node.type.isContainer {
                            // `.id(node.id)` forces SwiftUI to treat the
                            // section as a fresh view per selection. Without
                            // it, @State (metas, loadedCount, iterator)
                            // carries over from the previous selection, so
                            // the next render briefly shows the old prefix
                            // under the new node — and a stale LoadMoreStub
                            // can fire `loadNextPage` against the previous
                            // node's iterator before `.task` resets state,
                            // leaving the stub permanently stuck.
                            InspectorContainerSection(
                                node: node,
                                document: document,
                                selection: $selection
                            )
                            .id(node.id)
                        } else {
                            InspectorValueSection(node: node)
                        }
                        Divider().padding(.horizontal, 16)
                        InspectorMetadataSection(node: node, document: document)
                    }
                } else {
                    emptyStateContent
                }
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var emptyStateContent: some View {
        VStack(spacing: 12) {
            Image(systemName: "chevron.right.circle")
                .font(.system(size: 32, weight: .light))
                .foregroundStyle(.tertiary)
            Text("Select a node")
                .font(.headline)
                .foregroundStyle(.secondary)
            Text("Open a key in this view, or use Tree mode (⌘2)\nfor a hierarchical overview.")
                .font(.callout)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity, minHeight: 320)
        .padding(24)
    }
}
