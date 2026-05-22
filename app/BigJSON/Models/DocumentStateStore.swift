import Foundation
import Observation

/// Persists per-file UI state across launches. Stores the last query
/// for each URL keyed by absolute path.
@Observable
final class DocumentStateStore {
    struct State: Codable, Equatable {
        var query: String

        static let empty = State(query: "")
    }

    private static let storageKey = "BigJSON.documentStates.v1"
    private static let maxEntries = 64

    private var states: [String: State] = [:]
    private var lru: [String] = []

    init() {
        load()
    }

    func state(for url: URL) -> State {
        states[url.path] ?? .empty
    }

    func save(_ state: State, for url: URL) {
        let key = url.path
        states[key] = state
        lru.removeAll { $0 == key }
        lru.insert(key, at: 0)
        if lru.count > Self.maxEntries {
            for stale in lru.dropFirst(Self.maxEntries) {
                states.removeValue(forKey: stale)
            }
            lru = Array(lru.prefix(Self.maxEntries))
        }
        persist()
    }

    private func persist() {
        struct Persisted: Codable {
            var states: [String: State]
            var lru: [String]
        }
        let payload = Persisted(states: states, lru: lru)
        guard let data = try? JSONEncoder().encode(payload) else { return }
        UserDefaults.standard.set(data, forKey: Self.storageKey)
    }

    private func load() {
        struct Persisted: Codable {
            var states: [String: State]
            var lru: [String]
        }
        guard
            let data = UserDefaults.standard.data(forKey: Self.storageKey),
            let decoded = try? JSONDecoder().decode(Persisted.self, from: data)
        else {
            return
        }
        states = decoded.states
        lru = decoded.lru
    }
}
