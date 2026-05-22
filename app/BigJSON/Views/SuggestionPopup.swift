import SwiftUI

struct Suggestion: Hashable {
    let text: String
    let kind: Kind

    enum Kind: Hashable {
        case key
        case builtin
        case arrayAccessor

        var iconName: String {
            switch self {
            case .key:           "key"
            case .builtin:       "function"
            case .arrayAccessor: "list.bullet"
            }
        }

        var accent: Color {
            switch self {
            case .key:           .accentColor
            case .builtin:       .indigo
            case .arrayAccessor: .teal
            }
        }

        var label: String {
            switch self {
            case .key:           "key"
            case .builtin:       "fn"
            case .arrayAccessor: "arr"
            }
        }
    }
}

struct SuggestionPopup: View {
    let suggestions: [Suggestion]
    let selectedIndex: Int
    let onSelect: (Int) -> Void
    @State private var hoveredIndex: Int?

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(suggestions.indices, id: \.self) { i in
                        SuggestionRow(
                            suggestion: suggestions[i],
                            highlighted: i == selectedIndex,
                            hovered: hoveredIndex == i
                        )
                        .id(i)
                        .contentShape(Rectangle())
                        .onTapGesture { onSelect(i) }
                        .onHover { hoveredIndex = $0 ? i : (hoveredIndex == i ? nil : hoveredIndex) }
                    }
                }
            }
            .onChange(of: selectedIndex) { _, new in
                withAnimation(.linear(duration: 0.06)) {
                    proxy.scrollTo(new, anchor: .center)
                }
            }
        }
        .frame(width: 260)
        .frame(maxHeight: 260)
        .fixedSize(horizontal: true, vertical: true)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(.quaternary, lineWidth: 0.5)
        )
        .shadow(color: .black.opacity(0.18), radius: 10, x: 0, y: 4)
    }
}

private struct SuggestionRow: View {
    let suggestion: Suggestion
    let highlighted: Bool
    let hovered: Bool

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: suggestion.kind.iconName)
                .font(.system(size: 9))
                .foregroundStyle(iconColor)
                .frame(width: 12)
            Text(suggestion.text)
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(highlighted ? .white : .primary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Text(suggestion.kind.label)
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundStyle(highlighted ? .white.opacity(0.7) : .secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(rowBackground)
    }

    private var iconColor: Color {
        if highlighted { return .white }
        return suggestion.kind.accent
    }

    @ViewBuilder
    private var rowBackground: some View {
        if highlighted {
            Color.accentColor
        } else if hovered {
            Color.secondary.opacity(0.12)
        } else {
            Color.clear
        }
    }
}
