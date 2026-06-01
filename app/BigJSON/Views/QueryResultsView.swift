import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// Results panel — a vertical list of result rows that mirrors the
/// inspector's row chrome (`Inspector.TypeGlyph`, monospaced key + caption
/// secondary, pill-shaped hover/selection). Containers expand inline;
/// document-backed containers stream children through the engine's
/// resumable iterator, synthetic ones come pre-parsed from `fullText`.
struct QueryResultsView: View {
    let store: DocumentStore
    let onSelect: (UInt64) -> Void
    /// When set, an × button appears in the header. Used by the
    /// side-panel placement to clear the query and dismiss in one click.
    var onClose: (() -> Void)? = nil

    @State private var selection: NodePath?
    @State private var expanded: Set<NodePath> = []
    @State private var lazyState: [NodePath: LazyChildState] = [:]
    @State private var showStatsPopover: Bool = false
    /// Current view mode. Auto-set to the best match for the current
    /// result shape whenever results change; user can override per
    /// query via the segmented control in the header.
    @State private var mode: ResultsMode = .list
    /// Cached tabular snapshot for the current results, computed lazily
    /// when the user is actually viewing the table. Invalidated on
    /// results-change.
    @State private var tableSnapshot: TabularSnapshot?

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
        }
        .background(.background)
        .onAppear { refreshMode() }
        .onChange(of: store.query.results) { _, _ in refreshMode() }
        .onChange(of: store.query.text) { _, _ in
            // Wipe snapshot so we don't briefly render stale columns
            // against the new results before refreshMode rebuilds.
            tableSnapshot = nil
        }
    }

    /// Recomputes the table snapshot and picks the best mode for the
    /// current result shape. Called on appear and whenever the engine
    /// hands us a new result set.
    private func refreshMode() {
        let snap = TabularSnapshot.build(
            from: store.query.results,
            document: store.document
        )
        tableSnapshot = snap
        // Auto-mode: tabular if the snapshot's good enough. User's
        // explicit override survives until results change, since
        // refreshMode runs on results-change too — but a non-tabular
        // result set forcibly drops the user out of Table mode (the
        // table would be empty anyway).
        if snap.isTabular {
            mode = .table
        } else if mode == .table {
            mode = .list
        }
    }

    // MARK: Header

    @ViewBuilder
    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "list.bullet.rectangle")
                .foregroundStyle(.secondary)
                .font(.caption)
            Text("Results")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            if !headerCount.isEmpty {
                Text(headerCount)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.tertiary)
                // Subtle in-flight indicator. Appears only when a new
                // query is running *while* the previous results are
                // still on-screen — the empty-results case is handled
                // by `loadingPlaceholder` further down.
                if store.query.isRunning {
                    ProgressView()
                        .controlSize(.mini)
                        .help("Re-running query…")
                }
                if shouldShowStatsIcon {
                    Image(systemName: "info.circle")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .onHover { hovering in showStatsPopover = hovering }
                        .popover(isPresented: $showStatsPopover, arrowEdge: .bottom) {
                            QueryStatsPopover(
                                duration: store.query.duration,
                                resultCount: store.query.results.count,
                                hitLimit: store.query.hitLimit,
                                limitCap: 5000,
                                scannedRows: store.query.scannedRows,
                                lookupCalls: store.query.lookupCalls,
                                scannedBytes: store.query.scannedBytes,
                                document: store.document
                            )
                        }
                        .help("Query stats")
                }
            }
            Spacer()
            if !store.query.results.isEmpty, store.document != nil {
                Picker("View", selection: $mode) {
                    Image(systemName: "tablecells").tag(ResultsMode.table)
                    Image(systemName: "list.bullet").tag(ResultsMode.list)
                }
                .pickerStyle(.segmented)
                .fixedSize()
                .controlSize(.small)
                .help("View mode")

                if mode == .list, !expanded.isEmpty {
                    Button(action: collapseAll) {
                        Image(systemName: "chevron.up.chevron.down")
                            .font(.caption)
                    }
                    .buttonStyle(.borderless)
                    .help("Collapse all")
                }
                Menu {
                    Button("JSON Array…") { runExport(.jsonArray) }
                    Button("NDJSON…")     { runExport(.ndjson) }
                    Button("CSV…")        { runExport(.csv) }
                } label: {
                    Image(systemName: "square.and.arrow.up")
                        .font(.caption)
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("Export query results")
            }
            if let onClose {
                Button(action: onClose) {
                    Image(systemName: "xmark")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .help("Clear query and hide results panel")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(.bar)
    }

    private var headerCount: String {
        if store.query.error != nil { return "" }
        let n = store.query.results.count
        if n == 0 { return "" }
        let rowsLabel: String = store.query.hitLimit
            ? "\(Formatters.count(n))+ rows · limit reached"
            : "\(Formatters.count(n)) \(n == 1 ? "row" : "rows")"
        if let d = store.query.duration {
            return "\(rowsLabel) · \(Formatters.duration(d))"
        }
        return rowsLabel
    }

    private var shouldShowStatsIcon: Bool {
        store.query.error == nil
            && (!store.query.results.isEmpty || store.query.duration != nil)
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        let trimmed = store.query.text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            placeholder(
                title: "No query",
                description: "Type a filter above to query the document.",
                systemImage: "magnifyingglass"
            )
        } else if store.query.error != nil {
            placeholder(
                title: "Fix the query",
                description: "Results will update when the filter parses.",
                systemImage: "exclamationmark.triangle"
            )
        } else if store.query.results.isEmpty && store.query.isRunning {
            loadingPlaceholder
        } else if store.query.results.isEmpty {
            placeholder(
                title: "No matches",
                description: "The filter parsed but produced no rows.",
                systemImage: "tray"
            )
        } else {
            switch mode {
            case .table: tableContent
            case .list:  tree
            }
        }
    }

    /// Shown while the engine is actively running a query that hasn't
    /// produced its first row yet. Distinguishes "still working" from
    /// "finished with nothing to show" so users on long-running queries
    /// don't mistake the in-flight state for an empty result set.
    @ViewBuilder
    private var loadingPlaceholder: some View {
        VStack(spacing: 12) {
            ProgressView()
                .controlSize(.large)
            Text("Running…")
                .font(.headline)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var tableContent: some View {
        if let snap = tableSnapshot, !snap.columns.isEmpty {
            ResultsTableView(
                snapshot: snap,
                document: store.document,
                onOpenDocument: onSelect
            )
        } else {
            placeholder(
                title: "Not tabular",
                description: "These results don't share enough fields to render as a table. Switch to List view.",
                systemImage: "tablecells.badge.ellipsis"
            )
        }
    }

    @ViewBuilder
    private var tree: some View {
        let topRows = buildTopRows()
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 1) {
                ForEach(Array(topRows.enumerated()), id: \.offset) { _, row in
                    ResultRowView(
                        entry: row,
                        depth: 0,
                        selection: $selection,
                        expanded: $expanded,
                        lazyState: $lazyState,
                        onOpenDocument: onSelect
                    )
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 8)
        }
        .onChange(of: store.query.text) { _, _ in
            selection = nil
            expanded.removeAll()
            lazyState.removeAll()
        }
        .onChange(of: store.query.results) { _, _ in
            autoExpandIfSingle(topRows)
        }
        .onAppear { autoExpandIfSingle(topRows) }
        .onReceive(NotificationCenter.default.publisher(for: .bigJSONStepResult)) { note in
            guard let dir = note.userInfo?["direction"] as? Int else { return }
            stepSelection(by: dir, rows: topRows)
        }
    }

    // MARK: Top-row builder

    /// Walks the engine's flat `QueryResult` list and builds the top-
    /// level entries. Each entry carries its label, type, and either
    /// a scalar payload or a container source (eager for synthetic
    /// rows, lazy for document-backed). No wrapping — top-level rows
    /// are just rows.
    private func buildTopRows() -> [RowEntry] {
        store.query.results.enumerated().map { idx, r in
            entryForResult(r, index: idx)
        }
    }

    private func entryForResult(_ r: QueryResult, index: Int) -> RowEntry {
        let mode: RowMode = labelLooksNamed(r) ? .named(r.path) : .indexed(index)
        if let nid = r.nodeID, let doc = store.document,
           let eid = doc.engineNodeID(from: nid) {
            if r.type.isContainer {
                let total = doc.childCount(of: eid)
                let path = NodePath(segments: [singleSegment(forIndex: index, mode: mode)])
                return RowEntry(
                    mode: mode,
                    type: r.type,
                    payload: .container(
                        kind: r.type == .object ? .object : .array,
                        children: .lazy(LazyMeta(document: doc, engineID: eid, totalChildren: total))
                    ),
                    nodeID: nid,
                    path: path
                )
            }
            // Document-backed primitive.
            let path = NodePath(segments: [singleSegment(forIndex: index, mode: mode)])
            return RowEntry(
                mode: mode,
                type: r.type,
                payload: .scalar(r.fullText ?? r.preview),
                nodeID: nid,
                path: path
            )
        }
        // Synthetic — parse fullText as JSON; fall back to opaque scalar.
        let text = r.fullText ?? r.preview
        let path = NodePath(segments: [singleSegment(forIndex: index, mode: mode)])
        if let parsed = ResultsJSON.parse(text) {
            switch parsed {
            case .scalar(let t, let v):
                return RowEntry(mode: mode, type: t, payload: .scalar(v), nodeID: nil, path: path)
            case .container(let kind, let entries):
                return RowEntry(
                    mode: mode,
                    type: kind == .object ? .object : .array,
                    payload: .container(kind: kind, children: .eager(entries)),
                    nodeID: nil,
                    path: path
                )
            }
        }
        return RowEntry(mode: mode, type: r.type, payload: .scalar(text), nodeID: nil, path: path)
    }

    /// Whether a row's path reads as a user-meaningful name (aggregate
    /// output name or bucket key) or as a path/index from a stream.
    /// Named rows render with their label visible; indexed ones lean on
    /// the inspector's array-element layout (content fills the row).
    private func labelLooksNamed(_ r: QueryResult) -> Bool {
        if r.nodeID != nil { return false }
        if r.path.isEmpty { return false }
        if r.path.hasPrefix("(synthetic)") { return false }
        if r.path.contains(".") || r.path.contains("[") { return false }
        return true
    }

    private func singleSegment(forIndex index: Int, mode: RowMode) -> NodePath.Segment {
        switch mode {
        case .named(let n): return .key(n)
        case .indexed:      return .index(index)
        }
    }

    // MARK: Auto-expand + step

    private func autoExpandIfSingle(_ rows: [RowEntry]) {
        guard rows.count == 1, case .container = rows[0].payload else { return }
        expanded.insert(rows[0].path)
    }

    private func collapseAll() {
        expanded.removeAll()
    }

    /// Step through the visible top-level rows when the user hits ⌘J/⌘K.
    /// Walking the full expanded tree is more complexity than payoff
    /// for now — power users use the chevrons.
    private func stepSelection(by direction: Int, rows: [RowEntry]) {
        guard !rows.isEmpty else { return }
        let currentIdx: Int? = selection.flatMap { sel in
            rows.firstIndex(where: { $0.path == sel })
        }
        let nextIdx: Int
        if let i = currentIdx {
            nextIdx = (i + direction + rows.count) % rows.count
        } else {
            nextIdx = direction >= 0 ? 0 : rows.count - 1
        }
        let next = rows[nextIdx]
        selection = next.path
        if let nid = next.nodeID { onSelect(nid) }
    }

    // MARK: Placeholder

    private func placeholder(
        title: String,
        description: String,
        systemImage: String
    ) -> some View {
        VStack(spacing: 10) {
            Spacer()
            Image(systemName: systemImage)
                .font(.system(size: 28, weight: .light))
                .foregroundStyle(.tertiary)
            Text(title).foregroundStyle(.secondary)
            Text(description)
                .font(.callout)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 280)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: Export

    private func runExport(_ format: QueryExporter.Format) {
        guard let doc = store.document else { return }
        let panel = NSSavePanel()
        panel.canCreateDirectories = true
        panel.nameFieldStringValue = format.defaultName
        switch format {
        case .jsonArray:
            panel.allowedContentTypes = [.json]
        case .ndjson:
            panel.allowedContentTypes = [.plainText]
        case .csv:
            panel.allowedContentTypes = [.commaSeparatedText]
        }
        if panel.runModal() == .OK, let url = panel.url {
            // Re-run the query against the engine so the rendered
            // bytes come from `engine::render` (single source of truth)
            // rather than a parallel Swift formatter. The on-screen
            // results' engine handle is long gone by export time, and
            // the source is in the OS cache so the second run is cheap.
            guard let data = QueryExporter.export(
                query: store.query.text,
                document: doc,
                format: format
            ) else { return }
            try? data.write(to: url)
        }
    }
}

// MARK: - View mode

/// Which presentation to use for the current result set. The header's
/// segmented picker binds to this; `refreshMode()` picks a default
/// based on shape when results change.
enum ResultsMode: Hashable {
    /// Spreadsheet-style view with columns inferred from the union of
    /// top-level keys across rows. Good for scanning homogeneous lists.
    case table
    /// Row list with inline expansion. Good for small structured
    /// results (single object, aggregate-no-by, heterogeneous rows).
    case list
}

// MARK: - Tabular snapshot

/// Pre-computed materialisation of the result set as a table. Built
/// once per query result and cached on the view; the table view just
/// reads from it. Detection is also baked in via `isTabular` so the
/// auto-mode logic doesn't re-scan the rows.
struct TabularSnapshot {
    let columns: [ColumnSpec]
    let rows: [TabularRow]
    /// True when the snapshot looks legitimately tabular — at least
    /// two rows, each row contributing fields, and the columns cover a
    /// meaningful fraction of the per-row field set. Drives auto-mode
    /// selection in `refreshMode()`.
    let isTabular: Bool

    /// Cap on the number of columns surfaced even when the data has
    /// more keys. Past this the table chrome gets unusable; the long
    /// tail of low-frequency keys is hidden (still accessible by
    /// switching to List view).
    static let maxColumns = 20

    /// How many rows we sample to decide if the result set is tabular
    /// at all and to pick column ordering. Past the sample the
    /// snapshot still includes all rows — sampling is just for
    /// detection + column ordering, not for content.
    static let sampleSize = 200

    /// Builds the snapshot from raw `QueryResult`s. Walks each row,
    /// extracts its top-level fields into `TabularCell`s, accumulates
    /// per-column frequency, and decides if the result is tabular.
    static func build(
        from results: [QueryResult],
        document: Engine.Document?
    ) -> TabularSnapshot {
        if results.isEmpty {
            return TabularSnapshot(columns: [], rows: [], isTabular: false)
        }

        // Extract per-row top-level fields. Skip rows we can't break
        // down into named fields (scalars at the top level, error
        // payloads, etc.) — they don't contribute to the column set
        // but still get rendered as rows with all cells missing.
        var rows: [TabularRow] = []
        rows.reserveCapacity(results.count)
        var frequency: [String: Int] = [:]
        var order: [String: Int] = [:]  // first-seen index — tiebreaker
        var orderCounter = 0
        var fieldedRowCount = 0

        for r in results {
            let cells = extractCells(from: r, document: document)
            if !cells.isEmpty {
                fieldedRowCount += 1
                for (k, _) in cells {
                    frequency[k, default: 0] += 1
                    if order[k] == nil { order[k] = orderCounter; orderCounter += 1 }
                }
            }
            rows.append(TabularRow(
                id: r.id,
                nodeID: r.nodeID,
                label: rowLabel(for: r, fallbackIndex: rows.count),
                cells: cells
            ))
        }

        // Sort columns by frequency descending, ties broken by first-
        // seen order so the table doesn't shuffle visually when two
        // keys tie.
        let columns: [ColumnSpec] = frequency
            .map { (k, count) in ColumnSpec(id: k, name: k, frequency: count) }
            .sorted { (a, b) in
                if a.frequency != b.frequency { return a.frequency > b.frequency }
                return (order[a.name] ?? .max) < (order[b.name] ?? .max)
            }
            .prefix(maxColumns)
            .map { $0 }

        // Tabular if: 2+ rows, most of them have fields, and there's
        // at least one column that hits 60% of rows. A single rare
        // shared key isn't enough — that gets a useless one-column
        // table.
        let isTabular: Bool = {
            guard results.count >= 2 else { return false }
            guard fieldedRowCount >= max(2, results.count * 4 / 10) else { return false }
            guard let topFreq = columns.first?.frequency else { return false }
            return topFreq >= max(2, fieldedRowCount * 6 / 10)
        }()

        return TabularSnapshot(columns: columns, rows: rows, isTabular: isTabular)
    }

    /// Picks the identifier text for the row's "Name" column. Prefers
    /// the engine-emitted path when it carries a meaningful name
    /// (filter / bucket key / aggregate output name); falls back to a
    /// positional `[N]` index when the path is empty or a synthetic
    /// placeholder.
    private static func rowLabel(for r: QueryResult, fallbackIndex: Int) -> String {
        if !r.path.isEmpty && !r.path.hasPrefix("(synthetic)") {
            return r.path
        }
        return "[\(fallbackIndex)]"
    }

    /// Pulls the top-level fields off one result row into a map of
    /// `column name → cell`. Document-backed object rows go through
    /// `childrenMetaBatch`; synthetic rows lift `fullText` via the
    /// JSON parser. Non-object rows return empty.
    private static func extractCells(
        from r: QueryResult,
        document: Engine.Document?
    ) -> [String: TabularCell] {
        // Document-backed object → one FFI batch for the top-level
        // fields.
        if let nid = r.nodeID, let doc = document,
           let eid = doc.engineNodeID(from: nid),
           r.type == .object {
            // Cap children we'll fetch per row. A row with 200 keys
            // would slow detection to a crawl and we only show
            // `maxColumns` anyway.
            let count = doc.childCount(of: eid)
            let limit = min(count, maxColumns * 2)
            let metas = doc.childrenMetaBatch(of: eid, offset: 0, limit: limit)
            var out: [String: TabularCell] = [:]
            for m in metas {
                guard m.isObjectMember, let key = doc.keyString(meta: m) else { continue }
                out[key] = cellFromMeta(m, document: doc)
            }
            return out
        }
        // Synthetic row whose full_text is a JSON object — parse,
        // pull top-level fields.
        if r.nodeID == nil,
           let text = r.fullText,
           let parsed = ResultsJSON.parse(text),
           case .container(.object, let entries) = parsed {
            var out: [String: TabularCell] = [:]
            for (k, v) in entries {
                out[k] = cellFromNode(v)
            }
            return out
        }
        return [:]
    }

    private static func cellFromMeta(
        _ meta: Engine.Document.ChildMeta,
        document: Engine.Document
    ) -> TabularCell {
        let type: JSONNodeType =
            Engine.NodeKind(rawValue: meta.kind)?.toJSONNodeType() ?? .null
        if type.isContainer && !meta.isPrimitive {
            let kind: ContainerKind = type == .object ? .object : .array
            return .container(kind: kind, count: Int(meta.childCount),
                              nodeID: document.jsonNodeID(for: meta.id))
        }
        // Primitive — read a bounded preview.
        let budget = PreviewBudget.memberSecondary
        let text: String = {
            guard let bytes = budget.sourceBytes,
                  let r = document.valueStringPrefix(meta: meta, maxBytes: bytes)
            else { return "" }
            return r.text.truncated(toChars: budget.displayChars, force: r.truncated)
        }()
        return .scalar(type: type, text: text)
    }

    private static func cellFromNode(_ node: ResultsNode) -> TabularCell {
        switch node {
        case .scalar(let t, let v):
            return .scalar(type: t, text: v)
        case .container(let kind, let entries):
            return .container(kind: kind, count: entries.count, nodeID: nil)
        }
    }
}

struct ColumnSpec: Identifiable, Hashable {
    let id: String       // column name (unique)
    let name: String
    let frequency: Int   // how many rows have this column
}

struct TabularRow: Identifiable, Hashable {
    let id: UUID
    let nodeID: UInt64?
    /// Row identifier — usually the engine-emitted path (filter name,
    /// bucket key, array index path, …). Rendered as the table's
    /// first "Name" column so the row keeps its identity when cells
    /// are inferred from values only.
    let label: String
    let cells: [String: TabularCell]

    static func == (a: TabularRow, b: TabularRow) -> Bool { a.id == b.id }
    func hash(into h: inout Hasher) { h.combine(id) }
}

enum TabularCell: Hashable {
    case scalar(type: JSONNodeType, text: String)
    case container(kind: ContainerKind, count: Int, nodeID: UInt64?)
}

// MARK: - Table view

/// Spreadsheet view over a `TabularSnapshot`. Uses SwiftUI's native
/// `Table` so the chrome (sortable headers, resizable columns,
/// alternating rows, selection rendering) is all `NSTableView`-quality
/// rather than hand-rolled. Cells render scalars as type-colored
/// monospaced text and containers as compact `{ N }` / `[ N ]` chips
/// in the container's accent color.
private struct ResultsTableView: View {
    let snapshot: TabularSnapshot
    let document: Engine.Document?
    let onOpenDocument: (UInt64) -> Void

    @State private var selectedRow: UUID?

    var body: some View {
        Table(snapshot.rows, selection: $selectedRow) {
            TableColumn("Name") { (row: TabularRow) in
                Text(row.label)
                    .font(.system(.callout, design: .monospaced).weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            TableColumnForEach(snapshot.columns) { (col: ColumnSpec) in
                TableColumn(col.name) { (row: TabularRow) in
                    cell(for: row, columnName: col.name)
                }
            }
        }
        .tableStyle(.inset(alternatesRowBackgrounds: true))
        .onChange(of: selectedRow) { _, newID in
            guard let id = newID,
                  let row = snapshot.rows.first(where: { $0.id == id }),
                  let nid = row.nodeID
            else { return }
            onOpenDocument(nid)
        }
    }

    @ViewBuilder
    private func cell(for row: TabularRow, columnName: String) -> some View {
        if let value = row.cells[columnName] {
            CellRenderer(cell: value)
        } else {
            Text("")
        }
    }
}

/// Renders one cell. Scalars get type-colored text; containers get a
/// compact summary chip in the container's accent color.
private struct CellRenderer: View {
    let cell: TabularCell

    var body: some View {
        switch cell {
        case .scalar(_, let text):
            Text(text)
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.tail)
        case .container(let kind, let count, _):
            let label = kind == .object
                ? (count == 1 ? "key" : "keys")
                : (count == 1 ? "item" : "items")
            Text("\(kind.openBrace) \(Formatters.count(count)) \(label) \(kind.closeBrace)")
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
        }
    }
}

// MARK: - Row model

/// One row of the results view. `mode` decides which layout the row
/// takes — `named` is a key/value style, `indexed` mirrors the
/// inspector's array-element preview style.
struct RowEntry: Equatable {
    let mode: RowMode
    let type: JSONNodeType
    let payload: RowPayload
    let nodeID: UInt64?
    let path: NodePath

    static func == (a: RowEntry, b: RowEntry) -> Bool { a.path == b.path }
}

enum RowMode: Equatable {
    case named(String)
    case indexed(Int)
}

enum RowPayload {
    case scalar(String)
    case container(kind: ContainerKind, children: ContainerChildren)
}

enum ContainerKind: Equatable {
    case object
    case array

    var asNodeType: JSONNodeType { self == .object ? .object : .array }
    var openBrace: String { self == .object ? "{" : "[" }
    var closeBrace: String { self == .object ? "}" : "]" }
}

enum ContainerChildren {
    case eager([(String, ResultsNode)])
    case lazy(LazyMeta)
}

// Self-recursive (children inlined rather than routed through
// `ContainerChildren`). The mutual `ResultsNode` ⇄ `ContainerChildren`
// reference tripped a "circular reference" type-check error under the
// Swift compiler in CI; collapsing it to single self-recursion avoids
// the cycle. `ResultsJSON` only ever builds eager containers, so no
// lazy case is needed here.
indirect enum ResultsNode {
    case scalar(JSONNodeType, String)
    case container(ContainerKind, [(String, ResultsNode)])
}

struct LazyMeta {
    let document: Engine.Document
    let engineID: UInt32
    let totalChildren: Int
}

/// Snapshot of a lazy container's loaded children. Stored by value so
/// the SwiftUI dictionary write triggers a re-render; the iterator
/// and preview cache are class refs so progress survives the copy.
struct LazyChildState {
    static let pageSize = 500
    let iterator: Engine.Document.ChildrenIterator
    let total: Int
    let previewCache: ChildPreviewCache
    var metas: [Engine.Document.ChildMeta] = []
    var loaded: Int = 0

    init(meta: LazyMeta) {
        self.iterator = meta.document.childrenIterator(of: meta.engineID)
        self.total = meta.totalChildren
        self.previewCache = ChildPreviewCache()
    }

    mutating func loadInitial() {
        let n = min(Self.pageSize, total)
        if n <= 0 { return }
        let fetched = iterator.next(limit: n)
        metas = fetched
        loaded = fetched.count
    }

    mutating func loadMore() {
        if loaded >= total { return }
        let n = min(Self.pageSize, total - loaded)
        let fetched = iterator.next(limit: n)
        if fetched.isEmpty { return }
        metas.append(contentsOf: fetched)
        loaded += fetched.count
    }
}

// MARK: - Node path

/// Stable address for a node inside the results tree. Used as the
/// expansion-set key, selection identifier, and lazy-state lookup.
struct NodePath: Hashable {
    enum Segment: Hashable {
        case key(String)
        case index(Int)
    }
    let segments: [Segment]

    func appending(_ seg: Segment) -> NodePath {
        NodePath(segments: segments + [seg])
    }
}

// MARK: - Row view

/// One row in the result list. Same chrome as `InspectorChildRow` —
/// type glyph + monospaced key + caption secondary — plus a leading
/// chevron when the row is a container. Expanded containers render
/// their children below as siblings in the outer `LazyVStack`.
private struct ResultRowView: View {
    let entry: RowEntry
    let depth: Int
    @Binding var selection: NodePath?
    @Binding var expanded: Set<NodePath>
    @Binding var lazyState: [NodePath: LazyChildState]
    let onOpenDocument: (UInt64) -> Void

    @State private var isHovered: Bool = false

    private static let indentPerDepth: CGFloat = 14
    private static let chevronColumn: CGFloat = 12

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            rowButton
            if isExpanded, case .container(let kind, let children) = entry.payload {
                ChildrenList(
                    kind: kind,
                    children: children,
                    parentPath: entry.path,
                    depth: depth + 1,
                    selection: $selection,
                    expanded: $expanded,
                    lazyState: $lazyState,
                    onOpenDocument: onOpenDocument
                )
            }
        }
    }

    private var isExpanded: Bool { expanded.contains(entry.path) }
    private var isSelected: Bool { selection == entry.path }

    @ViewBuilder
    private var rowButton: some View {
        Button(action: handleTap) {
            HStack(spacing: 8) {
                indentSpace
                disclosureCell
                Inspector.TypeGlyph(type: entry.type, size: .small)
                payloadView
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(rowBackground)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
    }

    private var indentSpace: some View {
        Color.clear.frame(width: CGFloat(depth) * Self.indentPerDepth)
    }

    @ViewBuilder
    private var disclosureCell: some View {
        if isContainer {
            Button(action: toggleExpand) {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    .frame(width: Self.chevronColumn,
                           height: Self.chevronColumn)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        } else {
            Color.clear.frame(width: Self.chevronColumn)
        }
    }

    @ViewBuilder
    private var payloadView: some View {
        switch entry.mode {
        case .indexed:
            indexedPayload
            Spacer(minLength: 0)
        case .named(let name):
            Text(name)
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 4)
            namedSecondary
        }
    }

    /// Inspector-style array-element layout: no key on the left, the
    /// row's content takes the full remaining width.
    @ViewBuilder
    private var indexedPayload: some View {
        switch entry.payload {
        case .scalar(let v):
            Text(v)
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(entry.type.accentColor)
                .lineLimit(1)
                .truncationMode(.tail)
        case .container(let kind, let children):
            Text(containerSummary(kind: kind, children: children))
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(kind.asNodeType.accentColor)
                .lineLimit(1)
                .truncationMode(.tail)
        }
    }

    @ViewBuilder
    private var namedSecondary: some View {
        switch entry.payload {
        case .scalar(let v):
            Text(v)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
        case .container(let kind, let children):
            Text(containerSummary(kind: kind, children: children))
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
        }
    }

    private func containerSummary(kind: ContainerKind, children: ContainerChildren) -> String {
        let n: Int
        switch children {
        case .eager(let entries): n = entries.count
        case .lazy(let meta):     n = meta.totalChildren
        }
        let label = kind == .object
            ? (n == 1 ? "key" : "keys")
            : (n == 1 ? "item" : "items")
        return "\(kind.openBrace) \(Formatters.count(n)) \(label) \(kind.closeBrace)"
    }

    private var isContainer: Bool {
        if case .container = entry.payload { return true }
        return false
    }

    private func toggleExpand() {
        if isExpanded {
            expanded.remove(entry.path)
        } else {
            expanded.insert(entry.path)
        }
        selection = entry.path
    }

    private func handleTap() {
        selection = entry.path
        if let nid = entry.nodeID {
            onOpenDocument(nid)
        }
    }

    @ViewBuilder
    private var rowBackground: some View {
        if isHovered {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .fill(.quaternary.opacity(0.6))
        }
    }
}

// MARK: - Children list

/// Renders the children of an expanded container. Eager children come
/// from the parent's parsed structure; lazy children stream in via
/// `Engine.Document.ChildrenIterator`, paginated by a `LazyChildState`
/// held on the root view.
private struct ChildrenList: View {
    let kind: ContainerKind
    let children: ContainerChildren
    let parentPath: NodePath
    let depth: Int
    @Binding var selection: NodePath?
    @Binding var expanded: Set<NodePath>
    @Binding var lazyState: [NodePath: LazyChildState]
    let onOpenDocument: (UInt64) -> Void

    var body: some View {
        switch children {
        case .eager(let entries):
            ForEach(Array(entries.enumerated()), id: \.offset) { idx, pair in
                let (label, child) = pair
                let childPath = parentPath.appending(
                    kind == .object ? .key(label) : .index(idx)
                )
                let mode: RowMode = (kind == .object) ? .named(label) : .indexed(idx)
                let entry = entryForEager(node: child, mode: mode, path: childPath)
                ResultRowView(
                    entry: entry,
                    depth: depth,
                    selection: $selection,
                    expanded: $expanded,
                    lazyState: $lazyState,
                    onOpenDocument: onOpenDocument
                )
            }
        case .lazy(let meta):
            LazyChildrenView(
                meta: meta,
                parentPath: parentPath,
                kind: kind,
                depth: depth,
                selection: $selection,
                expanded: $expanded,
                lazyState: $lazyState,
                onOpenDocument: onOpenDocument
            )
        }
    }

    private func entryForEager(node: ResultsNode, mode: RowMode, path: NodePath) -> RowEntry {
        switch node {
        case .scalar(let t, let v):
            return RowEntry(mode: mode, type: t, payload: .scalar(v), nodeID: nil, path: path)
        case .container(let kind, let entries):
            return RowEntry(
                mode: mode,
                type: kind.asNodeType,
                payload: .container(kind: kind, children: .eager(entries)),
                nodeID: nil,
                path: path
            )
        }
    }
}

/// Wraps lazy-children pagination: creates the snapshot on first
/// appearance, drains it page-by-page as a load-more sentinel scrolls
/// into view.
private struct LazyChildrenView: View {
    let meta: LazyMeta
    let parentPath: NodePath
    let kind: ContainerKind
    let depth: Int
    @Binding var selection: NodePath?
    @Binding var expanded: Set<NodePath>
    @Binding var lazyState: [NodePath: LazyChildState]
    let onOpenDocument: (UInt64) -> Void

    var body: some View {
        let state = lazyState[parentPath]
        if let s = state {
            ForEach(Array(s.metas.enumerated()), id: \.offset) { idx, childMeta in
                let entry = entry(for: childMeta, at: idx, in: s)
                ResultRowView(
                    entry: entry,
                    depth: depth,
                    selection: $selection,
                    expanded: $expanded,
                    lazyState: $lazyState,
                    onOpenDocument: onOpenDocument
                )
            }
            if s.loaded < s.total {
                LoadMoreRow(
                    loaded: s.loaded,
                    total: s.total,
                    depth: depth
                ) {
                    var copy = s
                    copy.loadMore()
                    lazyState[parentPath] = copy
                }
            }
        } else {
            Color.clear
                .frame(height: 1)
                .onAppear {
                    var s = LazyChildState(meta: meta)
                    s.loadInitial()
                    lazyState[parentPath] = s
                }
        }
    }

    private func entry(
        for childMeta: Engine.Document.ChildMeta,
        at idx: Int,
        in state: LazyChildState
    ) -> RowEntry {
        let childType: JSONNodeType =
            Engine.NodeKind(rawValue: childMeta.kind)?.toJSONNodeType() ?? .null

        let mode: RowMode
        if childMeta.isObjectMember {
            mode = .named(meta.document.keyString(meta: childMeta) ?? "")
        } else {
            mode = .indexed(Int(childMeta.arrayIndex))
        }

        let childPath: NodePath = {
            switch mode {
            case .named(let n):  return parentPath.appending(.key(n))
            case .indexed(let i): return parentPath.appending(.index(i))
            }
        }()

        let nodeID: UInt64? = childMeta.isPrimitive
            ? nil
            : meta.document.jsonNodeID(for: childMeta.id)

        if childType.isContainer && !childMeta.isPrimitive {
            let kind: ContainerKind = childType == .object ? .object : .array
            return RowEntry(
                mode: mode,
                type: childType,
                payload: .container(
                    kind: kind,
                    children: .lazy(LazyMeta(
                        document: meta.document,
                        engineID: childMeta.id,
                        totalChildren: Int(childMeta.childCount)
                    ))
                ),
                nodeID: nodeID,
                path: childPath
            )
        }
        return RowEntry(
            mode: mode,
            type: childType,
            payload: .scalar(primitivePreview(of: childMeta)),
            nodeID: nodeID,
            path: childPath
        )
    }

    private func primitivePreview(of childMeta: Engine.Document.ChildMeta) -> String {
        let budget = PreviewBudget.memberSecondary
        guard let bytes = budget.sourceBytes,
              let r = meta.document.valueStringPrefix(meta: childMeta, maxBytes: bytes)
        else { return "" }
        return r.text.truncated(toChars: budget.displayChars, force: r.truncated)
    }
}

/// Lazy load-more sentinel. Fires `onAppear` once when scrolled into
/// view; reads the same indentation as a row at the parent's depth so
/// it feels like the next item in the list rather than a separate
/// affordance.
private struct LoadMoreRow: View {
    let loaded: Int
    let total: Int
    let depth: Int
    let onAppear: () -> Void

    @State private var didTrigger = false

    var body: some View {
        HStack(spacing: 8) {
            Color.clear
                .frame(width: CGFloat(depth) * 14 + 12)
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.6)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .onAppear {
            guard !didTrigger else { return }
            didTrigger = true
            onAppear()
        }
    }

    private var label: String {
        let remaining = max(0, total - loaded)
        let next = min(LazyChildState.pageSize, remaining)
        return "Loading next \(Formatters.count(next)) of \(Formatters.count(remaining)) more…"
    }
}

// MARK: - JSON parser

/// Order-preserving JSON parser used to lift synthetic `fullText`
/// strings into the tree model. `JSONSerialization` is order-losing
/// for objects, which matters for aggregate outputs the user wrote
/// in a specific order.
enum ResultsJSON {
    static func parse(_ s: String) -> ResultsNode? {
        var c = Cursor(s)
        c.skipWS()
        guard let n = parseValue(&c) else { return nil }
        c.skipWS()
        return n
    }

    private struct Cursor {
        let chars: [Character]
        var pos: Int = 0
        init(_ s: String) { self.chars = Array(s) }
        var atEnd: Bool { pos >= chars.count }
        var current: Character? { atEnd ? nil : chars[pos] }
        mutating func advance() -> Character? {
            guard !atEnd else { return nil }
            let ch = chars[pos]; pos += 1; return ch
        }
        mutating func skipWS() {
            while let ch = current, ch.isWhitespace { pos += 1 }
        }
        mutating func match(_ expected: Character) -> Bool {
            if current == expected { pos += 1; return true }
            return false
        }
        mutating func matchKeyword(_ kw: String) -> Bool {
            let kwChars = Array(kw)
            guard pos + kwChars.count <= chars.count else { return false }
            for i in 0..<kwChars.count {
                if chars[pos + i] != kwChars[i] { return false }
            }
            pos += kwChars.count
            return true
        }
    }

    private static func parseValue(_ c: inout Cursor) -> ResultsNode? {
        c.skipWS()
        guard let ch = c.current else { return nil }
        switch ch {
        case "{": return parseObject(&c)
        case "[": return parseArray(&c)
        case "\"":
            guard let s = parseString(&c) else { return nil }
            return .scalar(.string, "\"" + s + "\"")
        case "t":
            return c.matchKeyword("true") ? .scalar(.bool, "true") : nil
        case "f":
            return c.matchKeyword("false") ? .scalar(.bool, "false") : nil
        case "n":
            return c.matchKeyword("null") ? .scalar(.null, "null") : nil
        case "-", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9":
            return parseNumber(&c)
        default:
            return nil
        }
    }

    private static func parseObject(_ c: inout Cursor) -> ResultsNode? {
        guard c.match("{") else { return nil }
        var entries: [(String, ResultsNode)] = []
        c.skipWS()
        if c.match("}") { return .container(.object, entries) }
        while true {
            c.skipWS()
            guard let key = parseString(&c) else { return nil }
            c.skipWS()
            guard c.match(":") else { return nil }
            guard let value = parseValue(&c) else { return nil }
            entries.append((key, value))
            c.skipWS()
            if c.match(",") { continue }
            if c.match("}") { return .container(.object, entries) }
            return nil
        }
    }

    private static func parseArray(_ c: inout Cursor) -> ResultsNode? {
        guard c.match("[") else { return nil }
        var entries: [(String, ResultsNode)] = []
        c.skipWS()
        if c.match("]") { return .container(.array, entries) }
        var idx = 0
        while true {
            guard let value = parseValue(&c) else { return nil }
            entries.append(("\(idx)", value))
            idx += 1
            c.skipWS()
            if c.match(",") { continue }
            if c.match("]") { return .container(.array, entries) }
            return nil
        }
    }

    private static func parseString(_ c: inout Cursor) -> String? {
        guard c.match("\"") else { return nil }
        var out = String()
        while let ch = c.advance() {
            if ch == "\"" { return out }
            if ch == "\\" {
                guard let esc = c.advance() else { return nil }
                switch esc {
                case "\"": out.append("\"")
                case "\\": out.append("\\")
                case "/": out.append("/")
                case "b": out.append("\u{08}")
                case "f": out.append("\u{0C}")
                case "n": out.append("\n")
                case "r": out.append("\r")
                case "t": out.append("\t")
                case "u":
                    var code: UInt32 = 0
                    for _ in 0..<4 {
                        guard let d = c.advance(), let v = hexValue(d) else { return nil }
                        code = code * 16 + v
                    }
                    if let scalar = Unicode.Scalar(code) {
                        out.append(Character(scalar))
                    } else {
                        out.append("\u{FFFD}")
                    }
                default:
                    out.append(esc)
                }
            } else {
                out.append(ch)
            }
        }
        return nil
    }

    private static func hexValue(_ ch: Character) -> UInt32? {
        guard let scalar = ch.unicodeScalars.first else { return nil }
        let v = scalar.value
        if v >= 0x30 && v <= 0x39 { return v - 0x30 }
        if v >= 0x41 && v <= 0x46 { return v - 0x41 + 10 }
        if v >= 0x61 && v <= 0x66 { return v - 0x61 + 10 }
        return nil
    }

    private static func parseNumber(_ c: inout Cursor) -> ResultsNode? {
        let start = c.pos
        if c.match("-") { /* sign */ }
        while let ch = c.current, ch.isNumber { _ = c.advance() }
        if c.match(".") {
            while let ch = c.current, ch.isNumber { _ = c.advance() }
        }
        if c.match("e") || c.match("E") {
            _ = c.match("+") || c.match("-")
            while let ch = c.current, ch.isNumber { _ = c.advance() }
        }
        let raw = String(c.chars[start..<c.pos])
        if raw.isEmpty { return nil }
        return .scalar(.number, raw)
    }
}

// MARK: - Stats popover

private struct QueryStatsPopover: View {
    let duration: TimeInterval?
    let resultCount: Int
    let hitLimit: Bool
    let limitCap: Int
    let scannedRows: UInt64?
    let lookupCalls: UInt64?
    let scannedBytes: UInt64?
    let document: Engine.Document?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Query")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 14, verticalSpacing: 4) {
                if let d = duration {
                    statsRow("Elapsed", value: Formatters.duration(d))
                }
                if let scanned = scannedRows {
                    statsRow(
                        "Scanned",
                        value: "\(Formatters.count(Int(min(scanned, UInt64(Int.max))))) \(scanned == 1 ? "row" : "rows")"
                    )
                }
                if let bytes = scannedBytes, bytes > 0 {
                    statsRow("Bytes read", value: bytesScannedValue(bytes))
                }
                if let lookups = lookupCalls, lookups > 0 {
                    statsRow(
                        "Lookups",
                        value: Formatters.count(Int(min(lookups, UInt64(Int.max))))
                    )
                }
                statsRow(
                    "Output",
                    value: hitLimit
                        ? "\(Formatters.count(resultCount))+ rows · cap reached"
                        : "\(Formatters.count(resultCount)) \(resultCount == 1 ? "row" : "rows")"
                )
                statsRow("Row cap", value: Formatters.count(limitCap))
            }
            if let document {
                Divider()
                Text("Document")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .textCase(.uppercase)
                Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 14, verticalSpacing: 4) {
                    statsRow("File size", value: Formatters.bytes(document.fileSize))
                    statsRow("Total nodes", value: Formatters.count(document.totalNodeCount))
                }
            }
        }
        .padding(14)
        .frame(minWidth: 240, alignment: .leading)
    }

    @ViewBuilder
    private func statsRow(_ label: String, value: String) -> some View {
        GridRow {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.primary)
                .gridColumnAlignment(.leading)
        }
    }

    /// Formats `bytes` as a human-readable size and appends a
    /// "/ file_size (NN%)" tail when we know the document size.
    /// Lets the user read "is this query bandwidth-bound?" at a
    /// glance — values approaching 100% mean the engine touched
    /// most of the document regardless of how few rows survived.
    private func bytesScannedValue(_ bytes: UInt64) -> String {
        let head = Formatters.bytes(Int64(min(bytes, UInt64(Int64.max))))
        guard let doc = document, doc.fileSize > 0 else { return head }
        let total = UInt64(doc.fileSize)
        let pct = total > 0 ? Int((Double(bytes) / Double(total) * 100).rounded()) : 0
        let totalStr = Formatters.bytes(doc.fileSize)
        return "\(head) / \(totalStr) · \(pct)%"
    }
}
