import SwiftUI

struct InspectorValueSection: View {
    let node: JSONNode

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Inspector.sectionLabel("Value")
            valueBox
            HStack(spacing: 8) {
                Button(action: copyValue) {
                    Label("Copy value", systemImage: "doc.on.doc")
                }
                .controlSize(.small)
                .disabled(node.leafFullText == nil)
                Spacer(minLength: 0)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    @ViewBuilder
    private var valueBox: some View {
        if let value = node.leafFullText {
            Text(value)
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(valueColor)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(.quaternary.opacity(0.5))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(.quaternary, lineWidth: 0.5)
                )
        } else {
            Text("—")
                .foregroundStyle(.secondary)
        }
    }

    private var valueColor: Color {
        switch node.type {
        case .string: .blue
        case .number: .purple
        case .bool: .orange
        case .null: .secondary
        default: .primary
        }
    }

    private func copyValue() {
        guard let v = node.leafFullText else { return }
        Inspector.copyToPasteboard(v)
    }
}
