import SwiftUI

struct InspectorMetadataSection: View {
    let node: JSONNode
    let document: Engine.Document

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Inspector.sectionLabel("Metadata")
            VStack(alignment: .leading, spacing: 4) {
                MetadataRow(label: "Type",   value: node.type.label)
                if let engineID = document.engineNodeID(from: node.id) {
                    let offset = document.byteOffset(of: engineID)
                    let length = document.byteLength(of: engineID)
                    MetadataRow(
                        label: "Byte offset",
                        value: NumberFormatter.localizedString(
                            from: NSNumber(value: offset), number: .decimal
                        )
                    )
                    MetadataRow(
                        label: "Byte length",
                        value: ByteCountFormatter.string(
                            fromByteCount: Int64(length), countStyle: .file
                        )
                    )
                }
                if node.type.isContainer {
                    MetadataRow(label: "Children", value: "\(node.childCount)")
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .padding(.bottom, 8)
    }
}

struct MetadataRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(width: 96, alignment: .leading)
            Text(value)
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}
