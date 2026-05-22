import SwiftUI

struct EmptyStateView: View {
    let onOpen: () -> Void
    let recents: RecentFilesStore
    let onSelectRecent: (RecentFilesStore.Entry) -> Void

    var body: some View {
        VStack(spacing: 18) {
            Spacer(minLength: 40)
            Image(systemName: "doc.text.magnifyingglass")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(.secondary)
            Text("No file open")
                .font(.title2)
            Text("Drag a JSON file here, or press ⌘O.")
                .foregroundStyle(.secondary)
            Button(action: onOpen) {
                Text("Open JSON…")
            }
            .keyboardShortcut("o", modifiers: .command)
            .controlSize(.large)

            if !recents.entries.isEmpty {
                RecentFilesList(recents: recents, onSelect: onSelectRecent)
                    .padding(.top, 12)
            }

            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
        .overlay(alignment: .bottom) {
            Text("engine \(Engine.version)")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
                .padding(.bottom, 12)
        }
    }
}

private struct RecentFilesList: View {
    let recents: RecentFilesStore
    let onSelect: (RecentFilesStore.Entry) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text("Recent")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .textCase(.uppercase)
                Spacer()
            }
            VStack(spacing: 1) {
                ForEach(recents.entries.prefix(6)) { entry in
                    RecentFileRow(entry: entry) { onSelect(entry) }
                }
            }
            .padding(6)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        }
        .frame(maxWidth: 420)
    }
}

private struct RecentFileRow: View {
    let entry: RecentFilesStore.Entry
    let onSelect: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 8) {
                Image(systemName: "doc.text")
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.displayName)
                        .lineLimit(1)
                        .foregroundStyle(.primary)
                    Text(directoryDisplay)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(hovered ? AnyShapeStyle(.quaternary.opacity(0.6)) : AnyShapeStyle(Color.clear))
            )
        }
        .buttonStyle(.plain)
        .onHover { hovered = $0 }
    }

    private var directoryDisplay: String {
        let parent = (entry.displayPath as NSString).deletingLastPathComponent
        return abbreviateHome(parent)
    }

    private func abbreviateHome(_ path: String) -> String {
        let home = NSHomeDirectory()
        if path.hasPrefix(home) {
            return "~" + path.dropFirst(home.count)
        }
        return path
    }
}
