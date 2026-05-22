import Foundation
import Observation

/// State and behaviour for the query-bar autocomplete popup.
///
/// Owns the candidate list, dismissed/justTyped flags, and a
/// per-context cache of object keys + kinds. Keeps `QueryBarView`
/// out of the business of running FFI sample queries on every
/// keystroke.
///
/// Inputs (text, cursor, focus) flow in through the `handle*`
/// methods; outputs (suggestions, selectedIndex, popup visibility)
/// are read directly from the @Observable properties. Applying a
/// suggestion is a pure function over the current text + cursor —
/// the view does the actual `Binding` writes.
@Observable
@MainActor
final class QueryAutocompleteModel {
    private(set) var suggestions: [Suggestion] = []
    var selectedIndex: Int = 0

    /// True when the popup should stay hidden until the user types
    /// the next character. Set on Esc, on accepting a suggestion, on
    /// caret moves that aren't accompanied by a text change (mouse
    /// click, arrow key navigation), and at startup. Cleared by the
    /// text-change handler when the user actually types.
    var dismissedForCurrentText: Bool = true

    func shouldShowPopup(fieldFocused: Bool, isTextSearchMode: Bool) -> Bool {
        fieldFocused && !suggestions.isEmpty && !dismissedForCurrentText && !isTextSearchMode
    }

    /// Recompute suggestions without altering popup visibility. Use
    /// this on initial appear; text/cursor change handlers below adjust
    /// `dismissedForCurrentText` themselves.
    func refresh(text: String, cursor: Int, document: Engine.Document) {
        currentText = text
        currentCursor = cursor
        guard let derived = AutocompleteContext.derive(query: text, cursor: cursor) else {
            suggestions = []
            selectedIndex = 0
            return
        }

        let candidates: [Suggestion]
        switch derived.mode {
        case .fieldAccess(let context):
            // Field-access candidates depend on sampling the engine. The
            // sample queries themselves are slow on big files (each walks
            // up to 5000 outputs) — cache by context and kick the load
            // off a background task so the keystroke doesn't block.
            if let cached = contextCache[context] {
                candidates = fieldAccessCandidates(from: cached)
            } else {
                scheduleContextLoad(context: context, document: document)
                // Clear suggestions while the new context loads so we
                // don't show stale items from a previous scope/mode
                // (e.g. infix keywords lingering after the user types a
                // `.`). The async completion calls `refresh` again with
                // the populated cache.
                suggestions = []
                selectedIndex = 0
                return
            }
        case .builtin(.valueStart):
            candidates = AutocompleteContext.valueKeywords.map {
                Suggestion(text: $0, kind: .builtin)
            }
        case .builtin(.afterExpression):
            candidates = AutocompleteContext.infixKeywords.map {
                Suggestion(text: $0, kind: .builtin)
            }
        }

        let filtered: [Suggestion]
        if derived.partial.isEmpty {
            filtered = candidates
        } else {
            let needle = derived.partial.lowercased()
            filtered = candidates.filter { $0.text.lowercased().hasPrefix(needle) }
        }
        suggestions = Array(filtered.prefix(20))
        if !suggestions.indices.contains(selectedIndex) {
            selectedIndex = 0
        }
    }

    /// Called when the query text changes. A keystroke fires this
    /// observer first (text), then the cursor observer. The
    /// `justTyped` handshake lets the cursor observer know the move
    /// was caused by typing rather than navigation.
    func handleTextChange(text: String, cursor: Int, document: Engine.Document) {
        justTyped = true
        if ignoreNextChange {
            ignoreNextChange = false
        } else {
            dismissedForCurrentText = false
        }
        refresh(text: text, cursor: cursor, document: document)
    }

    /// Called when the caret moves. Without an accompanying text
    /// change, treat as navigation (mouse click, arrow keys) and keep
    /// the popup hidden — but still refresh candidates so the next
    /// typed character has the right context.
    func handleCursorChange(text: String, cursor: Int, document: Engine.Document) {
        if justTyped {
            justTyped = false
        } else {
            dismissedForCurrentText = true
        }
        refresh(text: text, cursor: cursor, document: document)
    }

    func dismiss() {
        if !dismissedForCurrentText {
            dismissedForCurrentText = true
        }
    }

    /// Result of applying a suggestion. The view writes both fields
    /// back through its own Bindings.
    struct ApplyResult {
        let newText: String
        let newCursor: Int
    }

    /// Splices the suggestion at `index` into `text` at `cursor`,
    /// replacing the surrounding identifier (whichever part of it sits
    /// before AND after the caret) so picking a completion in the
    /// middle of `.foo|bar` swaps the whole word instead of inserting
    /// the suggestion between halves.
    func applySuggestion(at index: Int, text: String, cursor: Int) -> ApplyResult? {
        guard suggestions.indices.contains(index) else { return nil }
        guard let derived = AutocompleteContext.derive(query: text, cursor: cursor) else { return nil }
        let s = suggestions[index]
        let queryNS = text as NSString
        let safeCursor = max(0, min(cursor, queryNS.length))
        let head = queryNS.substring(to: safeCursor)
        let tail = queryNS.substring(from: safeCursor)
        let headNS = head as NSString
        let partialLen = derived.partialUTF16Length
        var headWithoutPartial = headNS.substring(to: max(0, headNS.length - partialLen))

        // Drop the trailing identifier run from `tail` so applying a
        // suggestion mid-word replaces the whole word rather than
        // splicing into it.
        let trimmedTail = dropLeadingIdentifierChars(tail)

        let replacement: String
        switch s.kind {
        case .key:
            if Self.isSimpleIdentifier(s.text) {
                replacement = s.text
            } else {
                // Bracket form: drop the dot we'd otherwise leave
                // dangling.
                if headWithoutPartial.hasSuffix(".") {
                    headWithoutPartial = String(headWithoutPartial.dropLast())
                }
                replacement = "[\(Self.jsonStringLiteral(s.text))]"
            }
        case .arrayAccessor:
            // Array brackets attach directly to the previous segment
            // (`.users[]`, not `.users.[]`), so swallow the dot the
            // user typed to summon the popup.
            if headWithoutPartial.hasSuffix(".") {
                headWithoutPartial = String(headWithoutPartial.dropLast())
            }
            replacement = s.text
        case .builtin:
            replacement = s.text
        }

        let newText = headWithoutPartial + replacement + trimmedTail
        let newCursor = (headWithoutPartial as NSString).length + (replacement as NSString).length

        ignoreNextChange = true
        dismissedForCurrentText = true
        return ApplyResult(newText: newText, newCursor: newCursor)
    }

    /// Returns true if the key was consumed.
    func handleKey(
        _ action: AutocompleteTextField.KeyAction,
        fieldFocused: Bool,
        isTextSearchMode: Bool,
        text: String,
        cursor: Int,
        onApply: (ApplyResult) -> Void
    ) -> Bool {
        let visible = shouldShowPopup(fieldFocused: fieldFocused, isTextSearchMode: isTextSearchMode)
        switch action {
        case .arrowUp:
            guard visible, !suggestions.isEmpty else { return false }
            selectedIndex = (selectedIndex - 1 + suggestions.count) % suggestions.count
            return true
        case .arrowDown:
            guard visible, !suggestions.isEmpty else { return false }
            selectedIndex = (selectedIndex + 1) % suggestions.count
            return true
        case .tab, .enter:
            guard visible, suggestions.indices.contains(selectedIndex) else { return false }
            if let result = applySuggestion(at: selectedIndex, text: text, cursor: cursor) {
                onApply(result)
            }
            return true
        case .escape:
            guard visible else { return false }
            dismissedForCurrentText = true
            return true
        }
    }

    // MARK: Private

    /// Cached `(kinds, keys)` results per context-query string. Stable
    /// across context switches: typing past a scope and back uses the
    /// cached suggestions without re-running the FFI sample. Cleared
    /// only when the underlying document changes (see `clearCache`).
    private struct ContextEntry {
        let kinds: Set<JSONNodeType>
        let keys: [String]
    }
    private var contextCache: [String: ContextEntry] = [:]
    /// The context the most recent `refresh` saw — used by the async
    /// load completion to skip a stale refresh when the user has
    /// typed past the scope.
    private var lastRequestedContext: String = ""
    /// Context currently being sampled by `contextLoadTask`. Tracked
    /// separately so repeated keystrokes inside the same scope don't
    /// cancel the in-flight task and reset its debounce timer — that
    /// would prevent the load from ever completing on a fast typist.
    private var inFlightContext: String? = nil
    private var contextLoadTask: Task<Void, Never>? = nil
    /// Latest `(text, cursor)` the model has seen. Tracked so the
    /// async context load can re-run `refresh` against the current
    /// input — by the time the FFI sample returns the user has
    /// typically typed more, and refreshing against the snapshot we
    /// captured at schedule-time would show keys that don't match the
    /// current partial.
    private var currentText: String = ""
    private var currentCursor: Int = 0
    /// Skip the next text-change-driven popup-revive, so accepting a
    /// suggestion doesn't immediately re-open the popup.
    private var ignoreNextChange: Bool = false
    /// Set by `handleTextChange` and consumed by `handleCursorChange`,
    /// so a single keystroke (which fires both observers in declaration
    /// order) is treated as "user typed" rather than "cursor moved on
    /// its own".
    private var justTyped: Bool = false

    /// Array-accessor templates suggested when the current context is
    /// an array. `apply` strips the trailing dot before inserting these.
    private static let arrayAccessorSuggestions: [Suggestion] = [
        Suggestion(text: "[]", kind: .arrayAccessor),
        Suggestion(text: "[0]", kind: .arrayAccessor),
        Suggestion(text: "[-1]", kind: .arrayAccessor),
    ]

    /// Builds the suggestion list for a field-access scope from a
    /// cache entry. Objects contribute keys; arrays contribute
    /// bracket accessors. Mixed-kind scopes get both so the user can
    /// take either path.
    private func fieldAccessCandidates(from entry: ContextEntry) -> [Suggestion] {
        var out: [Suggestion] = []
        if entry.kinds.contains(.object) || (!entry.kinds.contains(.array) && !entry.keys.isEmpty) {
            out += entry.keys.map { Suggestion(text: $0, kind: .key) }
        }
        if entry.kinds.contains(.array) {
            out += Self.arrayAccessorSuggestions
        }
        return out
    }

    private func scheduleContextLoad(
        context: String,
        document: Engine.Document
    ) {
        lastRequestedContext = context
        // Already loading this exact context — don't cancel/reschedule.
        // Without this guard, typing inside the same scope (e.g. typing
        // `name` after `.foo.`) would cancel each in-flight load before
        // its debounce + FFI ever fires, so suggestions would never
        // appear for fast typists.
        if inFlightContext == context { return }
        contextLoadTask?.cancel()
        inFlightContext = context
        contextLoadTask = Task { @MainActor [weak self] in
            // Short debounce so fast-typed intermediate scopes don't
            // each fire an FFI sample. Kept tight so the popup feels
            // instant after a brief pause.
            try? await Task.sleep(for: .milliseconds(30))
            if Task.isCancelled { return }
            let result = await Task.detached(priority: .userInitiated) {
                (document.kindsForQuery(context), document.keysForQuery(context))
            }.value
            if Task.isCancelled { return }
            guard let self else { return }
            self.contextCache[context] = ContextEntry(kinds: result.0, keys: result.1)
            if self.inFlightContext == context {
                self.inFlightContext = nil
            }
            if self.lastRequestedContext == context {
                self.refresh(text: self.currentText, cursor: self.currentCursor, document: document)
            }
        }
    }

    /// Strips a leading run of identifier characters from `s`. Used
    /// when applying a suggestion mid-word so the rest of the word
    /// being typed gets replaced instead of being left behind.
    private func dropLeadingIdentifierChars(_ s: String) -> String {
        var idx = s.startIndex
        while idx < s.endIndex {
            let c = s[idx]
            if c.isLetter || c.isNumber || c == "_" {
                idx = s.index(after: idx)
            } else {
                break
            }
        }
        return String(s[idx...])
    }

    private static func isSimpleIdentifier(_ s: String) -> Bool {
        guard let first = s.first else { return false }
        if !first.isLetter && first != "_" { return false }
        for c in s.dropFirst() {
            if !c.isLetter && !c.isNumber && c != "_" { return false }
        }
        return true
    }

    private static func jsonStringLiteral(_ s: String) -> String {
        if let data = try? JSONSerialization.data(withJSONObject: s, options: [.fragmentsAllowed]),
           let str = String(data: data, encoding: .utf8) {
            return str
        }
        return "\"\(s)\""
    }
}
