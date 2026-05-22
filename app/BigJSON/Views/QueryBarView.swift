import SwiftUI
import AppKit

struct QueryBarView: View {
    @Bindable var store: DocumentStore
    let document: Engine.Document

    @State private var autocomplete = QueryAutocompleteModel()

    /// Caret position from the AppKit text field, in UTF-16 units.
    @State private var cursor: Int = 0
    @State private var fieldFocused: Bool = false

    /// Bumped to programmatically focus the text field. Cmd+F sends
    /// the `BigJSON.focusQuery` notification; we react by minting a
    /// new token, which AutocompleteTextField observes and turns into
    /// a `makeFirstResponder` call.
    @State private var focusToken: UUID? = nil

    /// Popover toggles for the recent / saved query lists.
    @State private var showRecentPopover: Bool = false
    @State private var showSavedPopover: Bool = false

    @State private var savedStore = SavedQueriesStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: queryIcon)
                    .foregroundStyle(isTextSearchMode ? Color.accentColor : .secondary)
                    .help(isTextSearchMode
                          ? "Plain-text search — case-insensitive, matches keys and primitive values."
                          : "SQL-shaped query — see the placeholder for syntax.")
                AutocompleteTextField(
                    text: $store.query.text,
                    cursor: $cursor,
                    isFocused: $fieldFocused,
                    placeholder: "/ for text search   ·   from .users as u where u.active count",
                    font: NSFont.monospacedSystemFont(
                        ofSize: NSFont.systemFontSize,
                        weight: .regular
                    ),
                    onKey: handleKey,
                    onClickOutside: { autocomplete.dismiss() },
                    focusToken: focusToken
                )
                // Lock vertical sizing to the field's intrinsic content
                // height — without this SwiftUI greedily expands the
                // NSViewRepresentable to the full HStack/parent height.
                .fixedSize(horizontal: false, vertical: true)
                Button(action: formatCurrentQuery) {
                    Image(systemName: "text.alignleft")
                }
                .controlSize(.small)
                .keyboardShortcut("l", modifiers: [.command, .option])
                .disabled(store.query.text.trimmingCharacters(in: .whitespaces).isEmpty)
                .help("Format query (⌥⌘L). Re-indents and lays out clauses.")
                Button(action: toggleSaveCurrentQuery) {
                    Image(systemName: isCurrentQuerySaved ? "bookmark.fill" : "bookmark")
                }
                .controlSize(.small)
                .keyboardShortcut("s", modifiers: .command)
                .disabled(store.query.text.trimmingCharacters(in: .whitespaces).isEmpty)
                .help(isCurrentQuerySaved
                      ? "Remove this query from saved (⌘S)."
                      : "Save this query as a named favourite (⌘S).")
                Button {
                    showSavedPopover.toggle()
                } label: {
                    Image(systemName: "books.vertical")
                }
                .controlSize(.small)
                .help("Saved queries")
                .popover(isPresented: $showSavedPopover, arrowEdge: .bottom) {
                    QueryListPopover(
                        title: "Saved",
                        icon: "bookmark.fill",
                        entries: savedStore.entries.map { QueryListEntry.saved($0) },
                        onSelect: { entry in
                            store.query.text = entry.query
                            showSavedPopover = false
                        },
                        onDelete: { entry in
                            savedStore.remove(id: entry.id)
                        },
                        onClearAll: { savedStore.clear() },
                        emptyMessage: "No saved queries yet — press ⌘S to save the current one."
                    )
                }
                Button {
                    showRecentPopover.toggle()
                } label: {
                    Image(systemName: "clock")
                }
                .controlSize(.small)
                .help("Recent queries")
                .popover(isPresented: $showRecentPopover, arrowEdge: .bottom) {
                    QueryListPopover(
                        title: "Recent",
                        icon: "clock",
                        entries: store.query.recentQueries.map { QueryListEntry.recent($0) },
                        onSelect: { entry in
                            store.query.text = entry.query
                            showRecentPopover = false
                        },
                        onDelete: { entry in
                            store.query.removeRecent(entry.query)
                        },
                        onClearAll: { store.query.clearRecent() },
                        emptyMessage: "No recent queries yet."
                    )
                }
            }
            .padding(.horizontal, 12)
            .padding(.top, 8)

            if let err = store.query.error {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text(err)
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                }
                .font(.callout)
                .padding(.horizontal, 12)
                .padding(.bottom, 6)
            } else {
                // `lookup(...)` indexes that aren't in the registry get
                // auto-built in `QueryModel.run`; the user never sees a
                // missing-index banner — indexes are assumed cheap and
                // get dropped when the query clears.
                Spacer().frame(height: 6)
            }
        }
        .overlay(alignment: .topLeading) {
            if popupVisible {
                SuggestionPopup(
                    suggestions: autocomplete.suggestions,
                    selectedIndex: autocomplete.selectedIndex,
                    onSelect: { idx in applySuggestion(at: idx) }
                )
                .offset(x: popupOffset.x, y: popupOffset.y)
                .transition(.opacity.combined(with: .move(edge: .top)))
                .zIndex(100)
            }
        }
        .animation(.easeOut(duration: 0.10), value: popupVisible)
        .onChange(of: store.query.text) { _, _ in
            store.scheduleQuery()
            autocomplete.handleTextChange(text: store.query.text, cursor: cursor, document: document)
        }
        .onChange(of: cursor) { _, _ in
            autocomplete.handleCursorChange(text: store.query.text, cursor: cursor, document: document)
        }
        .onAppear {
            autocomplete.refresh(text: store.query.text, cursor: cursor, document: document)
        }
        .onReceive(NotificationCenter.default.publisher(for: .bigJSONFocusQuery)) { _ in
            focusToken = UUID()
        }
    }

    private var popupVisible: Bool {
        autocomplete.shouldShowPopup(fieldFocused: fieldFocused, isTextSearchMode: isTextSearchMode)
    }

    private func handleKey(_ action: AutocompleteTextField.KeyAction) -> Bool {
        autocomplete.handleKey(
            action,
            fieldFocused: fieldFocused,
            isTextSearchMode: isTextSearchMode,
            text: store.query.text,
            cursor: cursor,
            onApply: writeApply
        )
    }

    private func applySuggestion(at index: Int) {
        guard let result = autocomplete.applySuggestion(
            at: index, text: store.query.text, cursor: cursor
        ) else { return }
        writeApply(result)
    }

    /// Commits an apply result into the bound text + cursor. Shared by
    /// both the keyboard path (Tab/Enter) and the mouse path (clicking
    /// a suggestion in the popup).
    private func writeApply(_ result: QueryAutocompleteModel.ApplyResult) {
        store.query.text = result.newText
        cursor = result.newCursor
    }

    /// True when the user is in plain-text search mode (`/foo`). The
    /// jq autocomplete popup is suppressed in this mode.
    private var isTextSearchMode: Bool {
        store.query.text.hasPrefix("/")
    }

    private var queryIcon: String {
        isTextSearchMode ? "magnifyingglass" : "terminal"
    }

    private func toggleSaveCurrentQuery() {
        let trimmed = store.query.text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        if savedStore.contains(query: trimmed) {
            savedStore.remove(query: trimmed)
        } else {
            savedStore.add(query: trimmed)
        }
    }

    /// Reformats the query in place. Silently no-ops on parse error
    /// so the user keeps editing — the existing parse-error banner is
    /// already surfacing the problem.
    private func formatCurrentQuery() {
        let trimmed = store.query.text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        guard let formatted = Engine.formatQuery(store.query.text) else { return }
        if formatted != store.query.text {
            store.query.text = formatted
        }
    }

    private var isCurrentQuerySaved: Bool {
        let trimmed = store.query.text.trimmingCharacters(in: .whitespaces)
        return !trimmed.isEmpty && savedStore.contains(query: trimmed)
    }

    /// X/Y offset for the popup, lining up its leading edge with where
    /// the partial begins on screen. X measures the current line only
    /// (so a multi-line query positions the popup under the caret's
    /// line, not at total-text width). Y bumps the popup down by one
    /// line-height per newline that precedes the partial.
    private var popupOffset: (x: CGFloat, y: CGFloat) {
        let baseX: CGFloat = 36
        let baseY: CGFloat = 34
        guard let derived = AutocompleteContext.derive(query: store.query.text, cursor: cursor) else {
            return (baseX, baseY)
        }
        let queryNS = store.query.text as NSString
        let safeCursor = max(0, min(cursor, queryNS.length))
        let upToPartialEnd = queryNS.substring(to: safeCursor) as NSString
        let upToPartialStart = upToPartialEnd.substring(
            to: max(0, upToPartialEnd.length - derived.partialUTF16Length)
        ) as NSString

        let font = NSFont.monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        let lineHeight = font.boundingRectForFont.height

        let lastNewline = upToPartialStart.range(of: "\n", options: .backwards)
        let currentLineStart = lastNewline.location == NSNotFound
            ? 0
            : lastNewline.location + 1
        let currentLineSegment = upToPartialStart.substring(from: currentLineStart) as NSString
        let xWidth = currentLineSegment.length == 0
            ? 0
            : currentLineSegment.size(withAttributes: [.font: font]).width

        var lineCount = 0
        for i in 0..<upToPartialStart.length {
            if upToPartialStart.character(at: i) == 0x0A { // '\n'
                lineCount += 1
            }
        }

        return (baseX + xWidth, baseY + CGFloat(lineCount) * lineHeight)
    }
}

