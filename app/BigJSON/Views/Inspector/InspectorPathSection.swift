import SwiftUI

struct InspectorPathSection: View {
    let node: JSONNode
    let document: Engine.Document
    @Binding var selection: JSONNode.ID?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Inspector.sectionLabel("Path")
            HStack(alignment: .top, spacing: 8) {
                BreadcrumbView(
                    node: node,
                    document: document,
                    selection: $selection
                )
                .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 2) {
                    Inspector.IconButton(systemName: "doc.on.doc", help: "Copy path") {
                        Inspector.copyToPasteboard(node.path)
                    }
                    Inspector.IconButton(systemName: "terminal", help: "Copy as jq command") {
                        let q = Inspector.shellQuote(node.path)
                        let p = Inspector.shellQuote(document.url.path(percentEncoded: false))
                        Inspector.copyToPasteboard("jq \(q) \(p)")
                    }
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }
}
