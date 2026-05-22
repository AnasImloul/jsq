import Foundation
import Observation

/// Persisted list of recently-opened files. Sandbox-friendly: each entry
/// carries a security-scoped bookmark so we can re-open the file on a
/// later launch without bouncing through the open panel.
@Observable
final class RecentFilesStore {
    struct Entry: Codable, Identifiable, Equatable {
        let displayPath: String
        let bookmark: Data
        let lastOpened: Date

        var id: String { displayPath }

        var displayName: String {
            (displayPath as NSString).lastPathComponent
        }
    }

    private static let storageKey = "BigJSON.recentFiles.v1"
    private static let maxEntries = 12

    var entries: [Entry] = []

    init() {
        load()
    }

    /// Records a successful open so the file appears in the menu. Best-effort
    /// — silently no-ops if the bookmark can't be created (e.g. URL not
    /// representing a file).
    func record(url: URL) {
        let path = url.path(percentEncoded: false)
        let bookmark: Data
        do {
            bookmark = try url.bookmarkData(
                options: [.withSecurityScope],
                includingResourceValuesForKeys: nil,
                relativeTo: nil
            )
        } catch {
            return
        }
        entries.removeAll { $0.displayPath == path }
        entries.insert(
            Entry(displayPath: path, bookmark: bookmark, lastOpened: Date()),
            at: 0
        )
        if entries.count > Self.maxEntries {
            entries = Array(entries.prefix(Self.maxEntries))
        }
        save()
    }

    /// Resolves the bookmark to a usable URL. Returns nil if the bookmark is
    /// no longer valid (file moved/deleted/permissions changed); the entry
    /// is dropped from the list in that case.
    func resolve(_ entry: Entry) -> URL? {
        var stale = false
        let url: URL
        do {
            url = try URL(
                resolvingBookmarkData: entry.bookmark,
                options: [.withSecurityScope],
                relativeTo: nil,
                bookmarkDataIsStale: &stale
            )
        } catch {
            remove(entry)
            return nil
        }
        if stale {
            // Try to refresh the bookmark in place
            if let refreshed = try? url.bookmarkData(
                options: [.withSecurityScope],
                includingResourceValuesForKeys: nil,
                relativeTo: nil
            ) {
                if let i = entries.firstIndex(where: { $0.id == entry.id }) {
                    entries[i] = Entry(
                        displayPath: entry.displayPath,
                        bookmark: refreshed,
                        lastOpened: entry.lastOpened
                    )
                    save()
                }
            }
        }
        return url
    }

    func remove(_ entry: Entry) {
        entries.removeAll { $0.id == entry.id }
        save()
    }

    func clear() {
        entries = []
        save()
    }

    private func save() {
        guard let data = try? JSONEncoder().encode(entries) else { return }
        UserDefaults.standard.set(data, forKey: Self.storageKey)
    }

    private func load() {
        guard
            let data = UserDefaults.standard.data(forKey: Self.storageKey),
            let decoded = try? JSONDecoder().decode([Entry].self, from: data)
        else {
            return
        }
        entries = decoded
    }
}
