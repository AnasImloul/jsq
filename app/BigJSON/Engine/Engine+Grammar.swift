import Foundation

/// Grammar manifest, tokeniser, completion context, query formatter,
/// and parse-progress polling. These are all process-wide concerns
/// that don't carry document state — they wrap `engine_*` calls that
/// take strings rather than `EngineDocument *`.
///
/// Ownership lives in Rust: the manifest is the single source of
/// truth for keyword vocabulary; the tokeniser drives syntax
/// highlighting; the completion context drives autocomplete. Swift
/// reads, never duplicates.
nonisolated extension Engine {
    struct GrammarManifest: Decodable, Sendable {
        let keywords: [Keyword]
        let operators: [Operator]
        let punctuation: [Punctuation]

        struct Keyword: Decodable, Sendable {
            let text: String
            let category: String   // clause | boolean | comparison | quantifier | sort | reducer | literal | builtin
            let role: String       // valueStart | afterExpression | both
        }
        struct Operator: Decodable, Sendable {
            let text: String
            let kind: String       // eq | ne | lt | le | gt | ge | assign
        }
        struct Punctuation: Decodable, Sendable {
            let text: String
            let kind: String       // dot | lbrack | ... | starStar
        }

        /// Keywords valid at the start of a fresh value expression
        /// (after `|`, `(`, `,`, comparison ops, `and`/`or`/`by`/`not`,
        /// or at the start of a query).
        var valueStartKeywords: [Keyword] {
            keywords.filter { $0.role == "valueStart" || $0.role == "both" }
        }
        /// Keywords valid only after a complete value expression — they
        /// combine the LHS with whatever the user types next.
        var afterExpressionKeywords: [Keyword] {
            keywords.filter { $0.role == "afterExpression" || $0.role == "both" }
        }
    }

    /// Lazy-loaded once per process. Decoded from `engine_grammar_manifest`.
    /// Both highlighter and autocomplete read from this instead of any
    /// Swift-side keyword tables.
    static let grammarManifest: GrammarManifest = {
        let bytes = engine_grammar_manifest()
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else {
            // Fallback to an empty manifest. Highlighter still works
            // (everything renders as identifier), autocomplete just
            // suggests nothing — better than crashing.
            return GrammarManifest(keywords: [], operators: [], punctuation: [])
        }
        let copy = Data(bytes: data, count: Int(bytes.length))
        do {
            return try JSONDecoder().decode(GrammarManifest.self, from: copy)
        } catch {
            return GrammarManifest(keywords: [], operators: [], punctuation: [])
        }
    }()

    enum TokenCategory: String, Sendable {
        case keyword, reducer, literal, identifier
        case string, number, comment
        case `operator`, splat, punctuation
        case error
    }

    struct Token: Sendable {
        let category: TokenCategory
        /// UTF-16 offset (NSRange-compatible).
        let offset: Int
        /// UTF-16 length (NSRange-compatible).
        let length: Int

        var nsRange: NSRange { NSRange(location: offset, length: length) }
    }

    /// Tokenises `source` for the highlighter. Calls into Rust's lexer,
    /// so any tokens the parser would recognise here also resolve in
    /// the engine — no drift possible. Forgiving — emits `error`
    /// tokens instead of failing on malformed input.
    static func tokenize(_ source: String) -> [Token] {
        let bytes = source.withCString { engine_tokenize($0) }
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else { return [] }
        let copy = Data(bytes: data, count: Int(bytes.length))
        struct Raw: Decodable {
            let category: String
            let offset: Int
            let length: Int
        }
        let raw: [Raw]
        do {
            raw = try JSONDecoder().decode([Raw].self, from: copy)
        } catch {
            return []
        }
        return raw.map { r in
            Token(
                category: TokenCategory(rawValue: r.category) ?? .error,
                offset: r.offset,
                length: r.length
            )
        }
    }

    enum CompletionMode: String, Sendable {
        case fieldAccess, valueStart, afterExpression
    }

    struct CompletionContext: Sendable {
        let mode: CompletionMode
        let partial: String
        let partialUtf16Length: Int
        /// Engine-evaluable query whose output is the input the
        /// pending field-access reads from. Only set for `.fieldAccess`.
        let contextQuery: String?
    }

    /// Classifies the cursor in `query` (`cursor` in UTF-16 / NSRange
    /// units) and returns the partial identifier, completion mode, and
    /// — for field access — an engine-evaluable expression whose output
    /// is the input the field-access will read from. Returns `nil` for
    /// cursor positions that don't admit completions.
    static func completionContext(query: String, cursor: Int) -> CompletionContext? {
        let bytes = query.withCString {
            engine_completion_context($0, UInt32(max(0, cursor)))
        }
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else { return nil }
        let copy = Data(bytes: data, count: Int(bytes.length))
        struct Raw: Decodable {
            let mode: String
            let partial: String
            let partialUtf16Length: Int
            let contextQuery: String?
        }
        let raw: Raw
        do {
            raw = try JSONDecoder().decode(Raw.self, from: copy)
        } catch {
            return nil
        }
        guard let mode = CompletionMode(rawValue: raw.mode) else { return nil }
        return CompletionContext(
            mode: mode,
            partial: raw.partial,
            partialUtf16Length: raw.partialUtf16Length,
            contextQuery: raw.contextQuery
        )
    }

    /// Re-formats a query string with canonical indentation. Returns
    /// the formatted text, or `nil` on parse error (the message is
    /// available via `engine_query_last_parse_error()` if needed).
    static func formatQuery(_ source: String) -> String? {
        let bytes = source.withCString { engine_format_query($0) }
        defer { engine_free_owned_bytes(bytes) }
        guard bytes.length > 0, let data = bytes.data else { return nil }
        let copied = Data(bytes: data, count: Int(bytes.length))
        return String(data: copied, encoding: .utf8)
    }

    /// Snapshot of the engine's current parse progress. `total` is
    /// zero before any document has started loading. While a load is
    /// in flight on another thread, polling this yields a determinate
    /// `parsed/total` fraction the UI can drive a progress bar with.
    struct ParseProgress: Equatable {
        let parsed: UInt64
        let total: UInt64
    }

    static func parseProgress() -> ParseProgress {
        var parsed: UInt64 = 0
        var total: UInt64 = 0
        engine_current_parse_progress(&parsed, &total)
        return ParseProgress(parsed: parsed, total: total)
    }
}
