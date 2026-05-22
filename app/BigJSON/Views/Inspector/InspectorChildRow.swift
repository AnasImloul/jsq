import SwiftUI

struct InspectorChildRow: View {
    let meta: Engine.Document.ChildMeta
    let rowKey: Int
    let document: Engine.Document
    let cache: ChildPreviewCache
    let isSelected: Bool
    let onTap: () -> Void

    /// Local hover state. Keeping it in the row prevents the parent
    /// from re-running its `body` (which walks the full `metas`
    /// ForEach) on every mouse-cross while scrolling.
    @State private var isHovered: Bool = false

    /// Cached depth-1 preview for array elements. Pre-populated from
    /// the parent cache at init, so a row scrolled back into view paints
    /// immediately without re-firing FFI.
    @State private var arrayPreview: ArrayPreview

    init(
        meta: Engine.Document.ChildMeta,
        rowKey: Int,
        document: Engine.Document,
        cache: ChildPreviewCache,
        isSelected: Bool,
        onTap: @escaping () -> Void
    ) {
        self.meta = meta
        self.rowKey = rowKey
        self.document = document
        self.cache = cache
        self.isSelected = isSelected
        self.onTap = onTap
        _arrayPreview = State(initialValue: cache.get(rowKey) ?? .empty)
    }

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: 8) {
                Inspector.TypeGlyph(type: kindAsType, size: .small)
                if meta.isArrayElement {
                    arrayPreviewView
                        .font(.system(.callout, design: .monospaced))
                    Spacer(minLength: 0)
                } else {
                    Text(keyLabel)
                        .font(.system(.callout, design: .monospaced))
                        .foregroundStyle(isSelected ? .white : .primary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    Text(memberSecondary)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(isSelected ? .white.opacity(0.8) : .secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(rowBackground)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
        .task(id: rowKey) {
            guard meta.isArrayElement else { return }
            if case .empty = arrayPreview {
                let computed = Self.depth1Preview(of: meta, document: document)
                cache.set(rowKey, computed)
                arrayPreview = computed
            }
        }
    }

    private var kindAsType: JSONNodeType {
        Engine.NodeKind(rawValue: meta.kind)?.toJSONNodeType() ?? .null
    }

    private var keyLabel: String {
        if meta.isObjectMember {
            return document.keyString(meta: meta) ?? ""
        }
        return "$"
    }

    private var memberSecondary: String {
        let kind = kindAsType
        if kind.isContainer {
            let n = Int(meta.childCount)
            if kind == .object {
                return "{ \(n) \(n == 1 ? "key" : "keys") }"
            }
            return "[ \(n) \(n == 1 ? "item" : "items") ]"
        }
        let budget = PreviewBudget.memberSecondary
        guard let bytes = budget.sourceBytes,
              let r = document.valueStringPrefix(meta: meta, maxBytes: bytes)
        else { return "" }
        return r.text.truncated(toChars: budget.displayChars, force: r.truncated)
    }

    // MARK: Depth-1 preview for array elements

    private static let maxPreviewKeys = 12

    struct PreviewPart {
        let key: String
        let value: String
        let valueKind: JSONNodeType
    }

    enum ArrayPreview {
        case empty
        case object(parts: [PreviewPart], hasMore: Bool)
        case array(text: String)
        case primitive(text: String, type: JSONNodeType)
    }

    @ViewBuilder
    private var arrayPreviewView: some View {
        switch arrayPreview {
        case .empty:
            Text(" ")
        case .object(let parts, let hasMore):
            // Brace framing as fixed-size, syntax-colored Texts; the
            // middle attributed Text owns the rest of the row width and
            // tail-truncates with "…" when it overflows.
            HStack(spacing: 0) {
                Text("{ ")
                    .fixedSize()
                    .foregroundStyle(isSelected ? Color.white : Self.braceColor)
                Text(buildObjectInner(parts: parts, hasMore: hasMore))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text(" }")
                    .fixedSize()
                    .foregroundStyle(isSelected ? Color.white : Self.braceColor)
            }
        case .array(let t):
            Text(t)
                .lineLimit(1)
                .truncationMode(.tail)
                .foregroundStyle(isSelected ? Color.white : JSONNodeType.array.accentColor)
        case .primitive(let text, let type):
            Text(text)
                .lineLimit(1)
                .truncationMode(.tail)
                .foregroundStyle(isSelected ? Color.white : type.accentColor)
        }
    }

    private func buildObjectInner(
        parts: [PreviewPart], hasMore: Bool
    ) -> AttributedString {
        let punctColor: Color = isSelected ? .white : Self.punctColor
        let keyColor: Color = isSelected ? .white : Self.keyColor

        var result = AttributedString()
        for (idx, p) in parts.enumerated() {
            if idx > 0 {
                var sep = AttributedString(", ")
                sep.foregroundColor = punctColor
                result += sep
            }
            var key = AttributedString(p.key)
            key.foregroundColor = keyColor
            result += key

            var colon = AttributedString(": ")
            colon.foregroundColor = punctColor
            result += colon

            var val = AttributedString(p.value)
            val.foregroundColor = isSelected ? .white : p.valueKind.accentColor
            result += val
        }
        if hasMore {
            var more = AttributedString(", …")
            more.foregroundColor = punctColor
            result += more
        }
        return result
    }

    // Frame around the inner key/value pairs. Type-specific colour for
    // the values themselves comes from `JSONNodeType.accentColor`.
    private static let braceColor: Color = .secondary
    private static let punctColor: Color = .secondary
    private static let keyColor: Color   = .primary

    /// Builds a depth-1 preview for an array element. Returns structured
    /// parts so the renderer can apply syntax colors per-piece.
    private static func depth1Preview(
        of meta: Engine.Document.ChildMeta,
        document: Engine.Document
    ) -> ArrayPreview {
        let kind = Engine.NodeKind(rawValue: meta.kind)?.toJSONNodeType() ?? .null
        switch kind {
        case .object:
            let totalKeys = Int(meta.childCount)
            // Primitives never have children; defensive check.
            if totalKeys == 0 || meta.isPrimitive {
                return .object(parts: [], hasMore: false)
            }
            let grand = document.childrenMetaBatch(
                of: meta.id, offset: 0, limit: maxPreviewKeys
            )
            let parts: [PreviewPart] = grand.map { c in
                let key = document.keyString(meta: c) ?? ""
                let cKind = Engine.NodeKind(rawValue: c.kind)?.toJSONNodeType() ?? .null
                let valueText: String
                switch cKind {
                case .object: valueText = "{…}"
                case .array:  valueText = "[…]"
                default:
                    let budget = PreviewBudget.nestedValue
                    let r = budget.sourceBytes.flatMap {
                        document.valueStringPrefix(meta: c, maxBytes: $0)
                    }
                    valueText = (r?.text ?? "").truncated(
                        toChars: budget.displayChars,
                        force: r?.truncated ?? false
                    )
                }
                return PreviewPart(key: key, value: valueText, valueKind: cKind)
            }
            return .object(parts: parts, hasMore: grand.count < totalKeys)
        case .array:
            let n = Int(meta.childCount)
            return .array(text: "Array of \(n) \(n == 1 ? "item" : "items")")
        default:
            let budget = PreviewBudget.primitiveArrayElement
            let r = budget.sourceBytes.flatMap {
                document.valueStringPrefix(meta: meta, maxBytes: $0)
            }
            return .primitive(text: r?.text ?? "", type: kind)
        }
    }

    @ViewBuilder
    private var rowBackground: some View {
        if isSelected {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(Color.accentColor)
        } else if isHovered {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(.quaternary.opacity(0.6))
        }
    }
}
