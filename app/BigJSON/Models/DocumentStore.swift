import Foundation
import AppKit
import Observation
import UniformTypeIdentifiers

/// User-chosen color scheme. `.system` defers to the macOS appearance.
/// Persisted via `@AppStorage("appTheme")` at the App scene level.
enum AppTheme: String, Hashable, Sendable, CaseIterable {
    case system
    case light
    case dark

    var label: String {
        switch self {
        case .system: "System"
        case .light:  "Light"
        case .dark:   "Dark"
        }
    }

    var systemImage: String {
        switch self {
        case .system: "circle.lefthalf.filled"
        case .light:  "sun.max"
        case .dark:   "moon"
        }
    }
}

/// Document lifecycle: Open Panel → background load → loaded handle.
/// Owns a `QueryModel` whose state is wired to the active document.
///
/// `@MainActor` is load-bearing. Without it, the `Task { … }` here and
/// in `QueryModel` run on the cooperative pool, and mutations to
/// `@Observable` state happen off the main thread. SwiftUI's
/// observation registrar isn't thread-safe, so concurrent reads from
/// view bodies and writes from those tasks could corrupt the tracking
/// dictionary, drop change notifications, or crash with
/// `EXC_BAD_ACCESS` under back-to-back queries.
@Observable
@MainActor
final class DocumentStore {
    enum LoadState: Equatable {
        case idle
        case loading(URL)
        case loaded(Engine.Document)
        case failed(message: String)

        static func == (lhs: LoadState, rhs: LoadState) -> Bool {
            switch (lhs, rhs) {
            case (.idle, .idle): true
            case (.loading(let a), .loading(let b)): a == b
            case (.loaded(let a), .loaded(let b)): a.url == b.url
            case (.failed(let a), .failed(let b)): a == b
            default: false
            }
        }
    }

    let documentStates: DocumentStateStore
    /// `var` (rather than `let`) so `@Bindable` can project a writable
    /// path into `query.text`. The reference itself is never reassigned
    /// — `init` is the only writer.
    var query: QueryModel

    init(documentStates: DocumentStateStore? = nil, query: QueryModel? = nil) {
        self.documentStates = documentStates ?? DocumentStateStore()
        self.query = query ?? QueryModel()
    }

    var state: LoadState = .idle

    var document: Engine.Document? {
        if case .loaded(let doc) = state { return doc }
        return nil
    }

    func showOpenPanel() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowedContentTypes = [.json]
        panel.title = "Open JSON file"
        panel.prompt = "Open"
        if panel.runModal() == .OK, let url = panel.url {
            open(url)
        }
    }

    func open(_ url: URL) {
        query.reset()
        state = .loading(url)
        Task { [weak self] in
            do {
                let doc = try await JSONLoader.load(url)
                guard let self else { return }
                self.bindQuery(to: doc)
                self.state = .loaded(doc)
                let saved = self.documentStates.state(for: url)
                if !saved.query.isEmpty {
                    self.query.text = saved.query
                }
                self.scheduleQuery()
            } catch {
                guard let self else { return }
                let msg = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
                self.state = .failed(message: msg)
            }
        }
    }

    /// Triggers the active document's run via the query model. Views
    /// call this from `onChange(of: store.query.text)`.
    func scheduleQuery() {
        query.schedule(against: document)
    }

    private func bindQuery(to document: Engine.Document) {
        let states = documentStates
        let url = document.url
        query.onTextPersist = { text in
            states.save(.init(query: text), for: url)
        }
    }
}
