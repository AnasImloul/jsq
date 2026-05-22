import Combine
import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @Bindable var store: DocumentStore
    let recents: RecentFilesStore
    @State private var selection: JSONNode.ID?

    var body: some View {
        body(for: store.state)
            .frame(minWidth: 960, minHeight: 560)
            .dropDestination(for: URL.self) { urls, _ in
                if let url = urls.first {
                    store.open(url)
                    return true
                }
                return false
            }
    }

    @ViewBuilder
    private func body(for state: DocumentStore.LoadState) -> some View {
        switch state {
        case .idle:
            EmptyStateView(
                onOpen: store.showOpenPanel,
                recents: recents,
                onSelectRecent: { entry in
                    if let url = recents.resolve(entry) {
                        store.open(url)
                    }
                }
            )
        case .loading(let url):
            // `.id(url)` so a back-to-back open of a different file
            // resets LoadingView's @State (rateAnchor / ETA), instead
            // of computing the new file's ETA against the previous
            // file's parsed-bytes anchor — which produced wildly
            // wrong "time remaining" readouts.
            LoadingView(url: url)
                .id(url)
        case .loaded(let doc):
            LoadedView(
                doc: doc,
                store: store,
                selection: $selection
            )
            .id(doc.url)
            .onCopyCommand {
                copyItems(in: doc)
            }
        case .failed(let message):
            ErrorView(message: message, onRetry: store.showOpenPanel)
        }
    }

    private func copyItems(in doc: Engine.Document) -> [NSItemProvider] {
        guard let id = selection, let node = doc.node(for: id) else { return [] }
        let text = node.leafFullText ?? node.path
        return [NSItemProvider(object: text as NSString)]
    }
}

private struct LoadedView: View {
    let doc: Engine.Document
    let store: DocumentStore
    @Binding var selection: JSONNode.ID?

    var body: some View {
        VStack(spacing: 0) {
            HeaderBar(title: doc.url.lastPathComponent)
            Divider()
            QueryBarView(store: store, document: doc)
                // `.id(doc.url)` resets QueryBarView's @State (cached
                // autocomplete keys/kinds for the current scope) when
                // a different document is opened — otherwise the
                // suggestion list would reflect the previous file's
                // schema until the user typed past the cached scope.
                .id(doc.url)
                .zIndex(10) // keep autocomplete popup above main view
            Divider()
            mainArea
                .zIndex(0)
            Divider()
            StatusBar(document: doc, selection: selection)
        }
        .onAppear {
            // Default to the document's root so the inspector has
            // something to show right away, and recover from a stale
            // selection left over from a previously-loaded document.
            let needsDefault: Bool
            if let current = selection {
                needsDefault = doc.engineNodeID(from: current) == nil
            } else {
                needsDefault = true
            }
            if needsDefault {
                selection = doc.jsonNodeID(for: doc.rootID)
            }
        }
    }

    /// Single inspector panel when there's no active query; a query
    /// makes the results panel slide in on the right with a draggable
    /// divider between the two.
    @ViewBuilder
    private var mainArea: some View {
        let queryActive = !store.query.text
            .trimmingCharacters(in: .whitespaces).isEmpty
        if queryActive {
            HSplitView {
                NavigationView(document: doc, selection: $selection)
                    .frame(minWidth: 320, idealWidth: 720, maxWidth: .infinity)
                QueryResultsView(
                    store: store,
                    onSelect: { selection = $0 },
                    onClose: { store.query.text = "" }
                )
                .frame(minWidth: 280, idealWidth: 420, maxWidth: .infinity)
            }
        } else {
            NavigationView(document: doc, selection: $selection)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

private struct LoadingView: View {
    let url: URL

    @State private var progress: Engine.ParseProgress = .init(parsed: 0, total: 0)
    /// Wall-clock anchor for ETA. Set on the first poll where `total`
    /// is non-zero — that's "the parser has actually started", since
    /// before then we'd be measuring rate over zero work.
    @State private var rateAnchor: (date: Date, bytes: UInt64)?

    var body: some View {
        VStack(spacing: 14) {
            Text("Loading \(url.lastPathComponent)…")
                .font(.callout)
            ProgressView(value: fraction)
                .progressViewStyle(.linear)
                .frame(width: 360)
            Text(detail)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onReceive(Timer.publish(every: 0.1, on: .main, in: .common).autoconnect()) { _ in
            let snapshot = Engine.parseProgress()
            progress = snapshot
            // Pin the rate anchor the moment the parser starts
            // reporting non-zero progress. Anchoring later (after a
            // few % already elapsed) makes the ETA stable rather
            // than yo-yoing during the first second.
            if rateAnchor == nil, snapshot.total > 0, snapshot.parsed > 0 {
                rateAnchor = (Date(), snapshot.parsed)
            }
        }
    }

    /// Fraction of bytes consumed, capped to [0, 1]. Falls back to an
    /// indeterminate-feeling 0 when the engine hasn't pulsed `total`
    /// yet (the cache-hit path skips the parser entirely).
    private var fraction: Double {
        guard progress.total > 0 else { return 0 }
        let frac = Double(progress.parsed) / Double(progress.total)
        return min(max(frac, 0), 1)
    }

    private var detail: String {
        guard progress.total > 0 else { return "preparing…" }
        let parsed = Formatters.bytes(Int64(progress.parsed))
        let total = Formatters.bytes(Int64(progress.total))
        let pct = Int((fraction * 100).rounded())
        var line = "\(parsed) of \(total)  ·  \(pct)%"

        // ETA from the anchored rate. Only show once we've collected
        // at least 0.5 s of samples — anything shorter is too noisy
        // to be useful as a "time remaining" claim.
        if let anchor = rateAnchor {
            let elapsed = -anchor.date.timeIntervalSinceNow
            let bytesSinceAnchor = progress.parsed > anchor.bytes
                ? progress.parsed - anchor.bytes
                : 0
            if elapsed >= 0.5, bytesSinceAnchor > 0, progress.total > progress.parsed {
                let rate = Double(bytesSinceAnchor) / elapsed   // bytes / sec
                let remaining = Double(progress.total - progress.parsed)
                let eta = remaining / rate
                line += "  ·  ~\(Formatters.duration(eta)) left"
            }
        }
        return line
    }
}

private struct ErrorView: View {
    let message: String
    let onRetry: () -> Void

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 44, weight: .light))
                .foregroundStyle(.orange)
            Text("Couldn't open file")
                .font(.title2)
            Text(message)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 480)
            Button("Try Another File…", action: onRetry)
                .keyboardShortcut("o", modifiers: .command)
                .controlSize(.large)
                .padding(.top, 4)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}
