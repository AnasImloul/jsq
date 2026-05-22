import SwiftUI

struct InspectorHeader: View {
    let node: JSONNode

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Inspector.TypeGlyph(type: node.type)
            VStack(alignment: .leading, spacing: 2) {
                Text(node.type.label)
                    .font(.title3.weight(.semibold))
                Text(summary)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
    }

    private var summary: String {
        switch node.type {
        case .object:
            let n = node.childCount
            return "\(n) \(n == 1 ? "key" : "keys")"
        case .array:
            let n = node.childCount
            return "\(n) \(n == 1 ? "item" : "items")"
        case .string:
            // Source span is (length - 2) inside the surrounding quotes.
            // Reading byteLength is one FFI call (O(1)); the previous
            // implementation loaded the entire value via leafFullText
            // and walked grapheme clusters on the main thread, freezing
            // the UI for seconds on multi-MB embedded blobs.
            let total = node.byteLength
            let inner = total >= 2 ? total - 2 : 0
            return ByteCountFormatter.string(
                fromByteCount: Int64(min(inner, UInt64(Int64.max))),
                countStyle: .file
            )
        case .number, .bool, .null:
            return node.leafFullText ?? ""
        }
    }
}
