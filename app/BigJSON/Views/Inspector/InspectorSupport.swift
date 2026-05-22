import SwiftUI
import AppKit

/// Inspector-scoped helpers and tiny UI primitives. Namespaced under
/// the `Inspector` enum so generic names (`IconButton`, `TypeGlyph`)
/// don't collide with future module-level types.
enum Inspector {
    static func sectionLabel(_ s: String) -> some View {
        Text(s)
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
            .textCase(.uppercase)
    }

    static func copyToPasteboard(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }

    static func shellQuote(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    struct IconButton: View {
        let systemName: String
        let help: String
        let action: () -> Void

        var body: some View {
            Button(action: action) {
                Image(systemName: systemName)
                    .font(.system(size: 12))
                    .frame(width: 22, height: 22)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .help(help)
        }
    }

    struct TypeGlyph: View {
        let type: JSONNodeType
        var size: GlyphSize = .large

        enum GlyphSize {
            case small, large
            var dimension: CGFloat { self == .large ? 32 : 18 }
            var fontSize: CGFloat { self == .large ? 11 : 8 }
            var corner: CGFloat { self == .large ? 6 : 3 }
        }

        var body: some View {
            Text(type.badge)
                .font(.system(size: size.fontSize, weight: .bold, design: .monospaced))
                .foregroundStyle(type.accentColor)
                .frame(width: size.dimension, height: size.dimension)
                .background(
                    RoundedRectangle(cornerRadius: size.corner, style: .continuous)
                        .fill(type.accentColor.opacity(0.18))
                )
        }
    }
}
