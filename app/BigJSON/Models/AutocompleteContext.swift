import Foundation

/// Cursor-aware autocomplete classification, sourced from the Rust
/// engine. `derive()` and the keyword lists are thin wrappers around
/// the FFI surface — see `Engine.completionContext` and
/// `Engine.grammarManifest`. The grammar (which strings count as
/// keywords, what counts as a value-start position, how to resolve a
/// field-access input) lives in `engine/src/query/grammar.rs` and
/// `engine/src/query/surface/completion.rs`. Everything in this file
/// is presentation glue.
nonisolated enum AutocompleteContext {
    enum Mode: Equatable {
        /// User is positioned after a `.` — suggest object keys at `context`.
        case fieldAccess(context: String)
        /// User is positioned where a builtin / keyword can start.
        case builtin(position: BuiltinPosition)
    }

    enum BuiltinPosition: Equatable {
        case valueStart
        case afterExpression
    }

    struct Derived: Equatable {
        let partial: String
        let partialUTF16Length: Int   // length of partial in UTF-16 units
        let mode: Mode
    }

    /// Identifier-prefix builtins valid where a fresh value expression
    /// is expected (after `|`, `(`, `,`, comparison ops, `and`/`or`/`by`
    /// /`not`, or at the start of input). Sourced from the engine's
    /// grammar manifest, so adding a keyword to the engine surfaces it
    /// here automatically.
    static var valueKeywords: [String] {
        Engine.grammarManifest.valueStartKeywords
            .map { $0.text }
            .sorted()
    }

    /// Identifier-prefix builtins valid only after a complete value
    /// expression. Same source-of-truth contract as `valueKeywords`.
    static var infixKeywords: [String] {
        Engine.grammarManifest.afterExpressionKeywords
            .map { $0.text }
            .sorted()
    }

    /// Union — used by callers that don't care about position.
    static var builtinNames: [String] {
        let all = Engine.grammarManifest.keywords.map { $0.text }
        return Array(Set(all)).sorted()
    }

    /// `cursor` is a UTF-16 offset (NSRange-style). Returns nil when
    /// the cursor isn't positioned for completion (e.g. mid-token after
    /// a number).
    static func derive(query: String, cursor: Int) -> Derived? {
        guard let ctx = Engine.completionContext(query: query, cursor: cursor) else {
            return nil
        }
        let mode: Mode
        switch ctx.mode {
        case .fieldAccess:
            // Engine guarantees `contextQuery` is set when mode is
            // fieldAccess; fall back to root if anything ever slipped.
            mode = .fieldAccess(context: ctx.contextQuery ?? ".")
        case .valueStart:
            mode = .builtin(position: .valueStart)
        case .afterExpression:
            mode = .builtin(position: .afterExpression)
        }
        return Derived(
            partial: ctx.partial,
            partialUTF16Length: ctx.partialUtf16Length,
            mode: mode
        )
    }
}
