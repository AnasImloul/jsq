import Foundation
import Observation

/// State and behaviour for the query bar. Holds the live text, the
/// most recent results, error / missing-index banners, the recent-
/// queries list, and the debounced run loop.
///
/// `DocumentStore` owns one of these, wires its `runner` to the
/// active document, and forwards lifecycle events (open, close).
/// Views read state directly from `store.query.*`.
@Observable
@MainActor
final class QueryModel {
    var text: String = ""
    var error: String?
    var results: [QueryResult] = []
    var hitLimit: Bool = false

    /// True while a query is actively executing in the engine (i.e.
    /// after the debounce, during the FFI call). Views read this to
    /// distinguish "no matches" from "query still running" — the latter
    /// shows a spinner instead of an empty-results placeholder.
    var isRunning: Bool = false

    /// Wall-clock time the most recent query (or text search) took to
    /// execute, measured around the FFI call. `nil` when nothing has
    /// run yet, or when the input is empty / failed to parse — those
    /// cases bypass the engine, so there's nothing meaningful to show.
    var duration: TimeInterval?

    /// How many rows the source path emitted before the rest of the
    /// pipeline (filter / aggregate / sort) ran. Surfaced in the stats
    /// popover so users can see "rows scanned" as the bottleneck
    /// proxy. `nil` while no query has run yet or after errors.
    var scannedRows: UInt64?

    /// Successful `lookup(...)` invocations during the most recent
    /// query. Surfaced alongside scanned rows so users can spot when
    /// a query has fanned a single source row out into many lookups.
    var lookupCalls: UInt64?

    /// Sum of source byte spans for every node the source path
    /// emitted. Compared against the document's file size in the
    /// stats popover to tell whether the query was memory-bandwidth-
    /// bound. `nil` when no query has run yet or after errors.
    var scannedBytes: UInt64?

    /// Set when the most recent query hit `lookup(SOURCE; KEY)` with
    /// no matching index in the registry. The query bar surfaces this
    /// as a banner with a "Create index" button. Reset on every run.
    var missingIndex: Engine.Document.MissingIndex?

    /// When the active query is a plain-text search, this is the
    /// needle (the part after the leading `/`). Result rows highlight
    /// occurrences of this string in path / value previews.
    var textSearchNeedle: String?

    /// MRU-first cached list of recent successful queries (max 20).
    /// Stored locally so SwiftUI views observe updates; persisted to
    /// UserDefaults via the wrapper helpers below.
    private(set) var recentQueries: [String] = {
        UserDefaults.standard.array(forKey: "recentQueries") as? [String] ?? []
    }()

    /// Per-document persistence hook. Set by `DocumentStore.open` so
    /// each `text` change writes-through to the document's saved state.
    @ObservationIgnored
    var onTextPersist: ((String) -> Void)?

    @ObservationIgnored private var queryTask: Task<Void, Never>?

    /// Indexes the model has auto-created on behalf of the current
    /// query (one entry per `lookup(SOURCE; KEY)` site the engine
    /// reported as missing). Dropped wholesale when the query goes
    /// empty — indexes are assumed cheap, so we don't keep them past
    /// the query that needed them.
    @ObservationIgnored private var autoCreatedIndexes: Set<AutoIndexKey> = []

    private struct AutoIndexKey: Hashable {
        let source: String
        let key: String
    }

    /// Reset all query-related state. Called when a new document is
    /// loaded so the previous document's results don't leak through.
    func reset() {
        queryTask?.cancel()
        text = ""
        clearOutputs()
    }

    private func clearOutputs() {
        results = []
        error = nil
        hitLimit = false
        missingIndex = nil
        duration = nil
        scannedRows = nil
        lookupCalls = nil
        scannedBytes = nil
        textSearchNeedle = nil
        isRunning = false
    }

    /// Debounced run. Called whenever `text` or the active document
    /// changes; runs the actual query off the main actor and publishes
    /// results back here.
    func schedule(against document: Engine.Document?) {
        queryTask?.cancel()
        let snapshot = text
        queryTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(150))
            if Task.isCancelled { return }
            await self?.run(text: snapshot, document: document)
        }
    }

    private func run(text: String, document: Engine.Document?, allowAutoIndex: Bool = true) async {
        // Persist the query as typed (even if empty / failing) so the
        // user gets back exactly what they had on next open.
        onTextPersist?(text)

        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || document == nil {
            // "Query finished" — drop any indexes we silently built
            // for the previous query before resetting state.
            if let document {
                dropAutoCreatedIndexes(in: document)
            }
            clearOutputs()
            return
        }
        guard let document else { return }

        if trimmed.hasPrefix("/") {
            await runTextSearch(rawText: text, needle: String(trimmed.dropFirst()), document: document)
            return
        }

        // Flip the spinner on right before the FFI call (after the
        // debounce + early-exit checks) so it only shows for queries
        // that actually reach the engine. We clear it on every exit
        // path below — success, failure, or auto-index re-run.
        isRunning = true

        let timed = await Task.detached(priority: .userInitiated) {
            let start = Date()
            let outcome = document.runQuery(trimmed, limit: 5000)
            return (outcome, -start.timeIntervalSinceNow)
        }.value
        if Task.isCancelled { isRunning = false; return }
        let (outcome, elapsed) = timed
        switch outcome {
        case .success(let runResult):
            // Auto-create any missing `lookup(...)` index inline.
            // Indexes are assumed cheap to build; we treat them as
            // implementation detail of running the query rather than
            // a user-facing decision. `allowAutoIndex` guards against
            // an infinite loop in the (unlikely) case the engine
            // re-reports the same hint after a successful create.
            if let missing = runResult.missingIndex, allowAutoIndex {
                let built = await Task.detached(priority: .userInitiated) {
                    document.createIndex(source: missing.source, key: missing.key)
                }.value
                if Task.isCancelled { isRunning = false; return }
                if built != nil {
                    autoCreatedIndexes.insert(
                        AutoIndexKey(source: missing.source, key: missing.key)
                    )
                    // Inner re-run will flip `isRunning` back on for its
                    // own FFI call; leaving it true here is correct.
                    await run(text: text, document: document, allowAutoIndex: false)
                    return
                }
            }

            isRunning = false
            error = nil
            results = runResult.results
            hitLimit = runResult.hitLimit
            missingIndex = runResult.missingIndex
            duration = elapsed
            scannedRows = runResult.scannedRows
            lookupCalls = runResult.lookupCalls
            scannedBytes = runResult.scannedBytes
            textSearchNeedle = nil
            if runResult.missingIndex == nil {
                recordRecent(text)
            }
        case .failure(let err):
            isRunning = false
            error = err.message
            results = []
            hitLimit = false
            missingIndex = nil
            duration = nil
            scannedRows = nil
            lookupCalls = nil
            scannedBytes = nil
            textSearchNeedle = nil
        }
    }

    /// `/` prefix triggers a substring search across keys + primitive
    /// values, bypassing the jq parser. Surfaces results without
    /// forcing the user to know any query language.
    private func runTextSearch(rawText: String, needle: String, document: Engine.Document) async {
        if needle.isEmpty {
            clearOutputs()
            return
        }
        isRunning = true
        let timed = await Task.detached(priority: .userInitiated) {
            let start = Date()
            let run = document.runTextSearch(needle, limit: 5000)
            return (run, -start.timeIntervalSinceNow)
        }.value
        if Task.isCancelled { isRunning = false; return }
        let (runResult, elapsed) = timed
        isRunning = false
        error = nil
        results = runResult.results
        hitLimit = runResult.hitLimit
        missingIndex = nil
        duration = elapsed
        scannedRows = runResult.scannedRows
        lookupCalls = runResult.lookupCalls
        scannedBytes = runResult.scannedBytes
        textSearchNeedle = needle
        recordRecent(rawText)
    }

    /// Drops every index this model created on behalf of the current
    /// query. Fired when the query text becomes empty — indexes are
    /// assumed cheap, so we don't keep them past the run that needed
    /// them. Indexes that were registered by other code paths (e.g.
    /// the user clicking "create index" elsewhere) are not touched
    /// because they were never added to `autoCreatedIndexes`.
    private func dropAutoCreatedIndexes(in document: Engine.Document) {
        if autoCreatedIndexes.isEmpty { return }
        let pending = autoCreatedIndexes
        autoCreatedIndexes.removeAll()
        Task.detached(priority: .utility) {
            for entry in pending {
                _ = document.dropIndex(source: entry.source, key: entry.key)
            }
        }
    }

    // MARK: Recent queries

    private func recordRecent(_ q: String) {
        let trimmed = q.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        var next = recentQueries
        next.removeAll { $0 == trimmed }
        next.insert(trimmed, at: 0)
        if next.count > Self.recentCap { next = Array(next.prefix(Self.recentCap)) }
        recentQueries = next
        UserDefaults.standard.set(next, forKey: "recentQueries")
    }

    func removeRecent(_ q: String) {
        recentQueries.removeAll { $0 == q }
        UserDefaults.standard.set(recentQueries, forKey: "recentQueries")
    }

    func clearRecent() {
        recentQueries = []
        UserDefaults.standard.removeObject(forKey: "recentQueries")
    }

    private static let recentCap = 20
}
