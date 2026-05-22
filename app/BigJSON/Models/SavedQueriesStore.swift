import Foundation
import Observation

/// A small list of named queries the user has bookmarked, persisted to
/// `UserDefaults`. Singleton because it's app-wide state shared across
/// every loaded document.
@Observable
final class SavedQueriesStore {
    static let shared = SavedQueriesStore()

    struct Entry: Identifiable, Codable, Hashable {
        var id: UUID
        var query: String
        /// Optional human label. If nil the UI falls back to the query
        /// text, truncated.
        var name: String?
    }

    private(set) var entries: [Entry] = []

    private static let storageKey = "savedQueries.v1"

    private init() {
        load()
    }

    func add(query: String, name: String? = nil) {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        // De-dupe on the query text — saving the same filter twice just
        // bumps it to the top instead of accumulating duplicates.
        entries.removeAll { $0.query == trimmed }
        entries.insert(Entry(id: UUID(), query: trimmed, name: name), at: 0)
        save()
    }

    func rename(id: UUID, to newName: String) {
        guard let i = entries.firstIndex(where: { $0.id == id }) else { return }
        let trimmed = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        entries[i].name = trimmed.isEmpty ? nil : trimmed
        save()
    }

    func remove(id: UUID) {
        entries.removeAll { $0.id == id }
        save()
    }

    func remove(query: String) {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        entries.removeAll { $0.query == trimmed }
        save()
    }

    func contains(query: String) -> Bool {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return entries.contains { $0.query == trimmed }
    }

    func clear() {
        entries = []
        save()
    }

    private func load() {
        guard let data = UserDefaults.standard.data(forKey: Self.storageKey) else { return }
        if let decoded = try? JSONDecoder().decode([Entry].self, from: data) {
            entries = decoded
        }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(entries) {
            UserDefaults.standard.set(data, forKey: Self.storageKey)
        }
    }
}
