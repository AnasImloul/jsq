import Foundation

nonisolated enum JSONLoader {
    /// Opens the JSON file via the Rust engine. Parsing happens off
    /// the main actor so the UI stays responsive on large files.
    static func load(_ url: URL) async throws -> Engine.Document {
        try await Task.detached(priority: .userInitiated) {
            try Engine.Document(opening: url)
        }.value
    }
}
