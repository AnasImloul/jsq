import SwiftUI

/// Custom header bar shown above the query bar for loaded documents.
/// Title is genuinely window-width centered (ZStack layer, full width),
/// picker sits at the trailing edge with explicit padding. Padding on
/// all sides is controlled here — no reliance on NSToolbar geometry.
struct HeaderBar: View {
    let title: String
    @AppStorage("appTheme") private var appThemeRaw: String = AppTheme.system.rawValue

    private var appTheme: AppTheme {
        AppTheme(rawValue: appThemeRaw) ?? .system
    }

    var body: some View {
        ZStack {
            HStack(spacing: 6) {
                Image(systemName: "doc.text")
                    .foregroundStyle(.secondary)
                Text(title)
                    .fontWeight(.medium)
            }
            .font(.callout)
            .frame(maxWidth: .infinity)

            HStack {
                Spacer()
                ThemeMenu(
                    selected: appTheme,
                    onSelect: { appThemeRaw = $0.rawValue }
                )
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

private struct ThemeMenu: View {
    let selected: AppTheme
    let onSelect: (AppTheme) -> Void

    var body: some View {
        Menu {
            ForEach(AppTheme.allCases, id: \.self) { theme in
                Button {
                    onSelect(theme)
                } label: {
                    if theme == selected {
                        Label(theme.label, systemImage: "checkmark")
                    } else {
                        Label(theme.label, systemImage: theme.systemImage)
                    }
                }
            }
        } label: {
            Image(systemName: selected.systemImage)
                .font(.callout)
                .frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Appearance — currently \(selected.label)")
    }
}
