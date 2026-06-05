//! Cursor-aware autocomplete classifier. Given a query source and a
//! cursor position (UTF-16 offset, JS string-index compatible), returns:
//!
//! * the partial identifier the user is typing,
//! * what kind of completion makes sense at this position
//!   (`FieldAccess` / `ValueStart` / `AfterExpression`),
//! * for field access, an engine-evaluable query whose output is the
//!   input the pending field-access will read from — fed back into
//!   `engine_keys_for_query` to fetch live key suggestions.
//!
//! Implementation reuses the engine's real lexer so the classification
//! tracks the surface grammar exactly. The earlier character-based
//! walker drifted from the parser (string contents leaked into the
//! returned context query, `where`/`select`/`aggregate` clauses weren't
//! peeled, etc.) — re-using the lexer eliminates that drift surface.

use super::super::grammar::{self, kw};
use super::super::lexer::{tokenize, Token, TokenKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionMode {
    FieldAccess,
    ValueStart,
    AfterExpression,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionContext {
    pub mode: CompletionMode,
    pub partial: String,
    pub partial_utf16_length: u32,
    /// Only set for `FieldAccess`. The engine query whose output is
    /// the input the pending field-access will read from. Empty path
    /// (`.`) when the cursor is at the document root.
    pub context_query: Option<String>,
}

/// Classifies the cursor position. Returns `None` when the cursor is
/// inside an unterminated string literal or in the middle of a number,
/// where suggesting completions would be wrong.
pub fn classify(source: &str, cursor_utf16: u32) -> Option<CompletionContext> {
    let head = truncate_at_utf16(source, cursor_utf16);
    let head_chars: Vec<char> = head.chars().collect();

    // Walk back through trailing identifier characters to extract the
    // partial the user is typing. A partial must start with a letter or
    // underscore — a leading digit means we're mid-number, which is not
    // a completion site.
    let mut partial_start = head_chars.len();
    while partial_start > 0 {
        let c = head_chars[partial_start - 1];
        if c.is_alphanumeric() || c == '_' {
            partial_start -= 1;
        } else {
            break;
        }
    }
    if partial_start < head_chars.len() {
        let first = head_chars[partial_start];
        if !(first.is_alphabetic() || first == '_') {
            return None;
        }
    }
    let partial: String = head_chars[partial_start..].iter().collect();
    let partial_utf16_length: u32 = partial.encode_utf16().count() as u32;

    let pre_partial: String = head_chars[..partial_start].iter().collect();

    // Bail inside an unterminated string — the cursor is part of string
    // content, not query syntax.
    if inside_open_string(&pre_partial) {
        return None;
    }

    let tokens = lenient_tokenize(&pre_partial);
    let last_kind = tokens.last().map(|t| t.kind.clone());

    if matches!(last_kind, Some(TokenKind::Dot)) {
        let context_query = compute_context_query(&tokens, &pre_partial);
        return Some(CompletionContext {
            mode: CompletionMode::FieldAccess,
            partial,
            partial_utf16_length,
            context_query: Some(context_query),
        });
    }

    if is_value_start(last_kind.as_ref()) {
        return Some(CompletionContext {
            mode: CompletionMode::ValueStart,
            partial,
            partial_utf16_length,
            context_query: None,
        });
    }

    Some(CompletionContext {
        mode: CompletionMode::AfterExpression,
        partial,
        partial_utf16_length,
        context_query: None,
    })
}

/// True when `last` ends a position from which a fresh value expression
/// can begin. The empty-token case (start of input) is value-start too.
fn is_value_start(last: Option<&TokenKind>) -> bool {
    let Some(t) = last else { return true };
    use TokenKind::*;
    match t {
        Pipe | LParen | LBrack | LBrace | Comma | Semi | Colon | Eq | Ne | Lt | Le | Gt | Ge
        | Assign => true,
        Ident(s) => is_value_intro_keyword(s),
        _ => false,
    }
}

/// Identifiers whose grammatical successor is a fresh value expression.
fn is_value_intro_keyword(s: &str) -> bool {
    matches!(
        s,
        "and"
            | "or"
            | "not"
            | "by"
            | "where"
            | "select"
            | "aggregate"
            | "collect"
            | "order"
            | "limit"
            | "from"
            | "join"
            | "on"
            | "as"
            | "let"
            | "fields"
            | "in"
            | "matches"
            | "starts_with"
            | "ends_with"
            | "contains"
    )
}

/// Clause keywords that take a body expression which sees the same
/// input as the keyword's source. Peeling the keyword resolves the
/// body's input to the upstream.
fn is_clause_with_body(s: &str) -> bool {
    matches!(
        s,
        "where"
            | "select"
            | "aggregate"
            | "limit"
            | "distinct"
            | "from"
            | "join"
            | "on"
            | "collect"
            | "order"
    )
}

/// Pattern-comparison style keywords — `EXPR matches PAT`, etc. Both
/// sides see the same input, so we peel the keyword and the LHS like a
/// regular comparison.
fn is_pattern_comparison(s: &str) -> bool {
    matches!(
        s,
        "matches" | "starts_with" | "ends_with" | "contains" | "in"
    )
}

/// Walks the source up to (but not including) the trailing dot and
/// returns an engine-evaluable query whose output is the upstream of
/// the field-access the user is typing.
///
/// The strategy: iteratively peel "transparent" prefixes (function
/// wrappers, pipes, comparison operators, clause keywords) off the
/// trailing position until what remains is a self-contained expression
/// that produces the input the field-access reads from.
///
/// `tokens` is the full lexed prefix including the trailing dot.
fn compute_context_query(tokens: &[Token], pre_partial: &str) -> String {
    if tokens.is_empty() {
        return ".".to_string();
    }
    // Drop the trailing dot — we don't want it in the upstream.
    let mut end = tokens.len() - 1;

    let mut changed = true;
    while changed && end > 0 {
        changed = false;

        // 1. Unmatched `(` at end of slice — paren-wrap or function
        //    call. The inner expression sees the same input as the
        //    surrounding call, so peel the `(` and any preceding
        //    identifier (the function name).
        if let Some(open_idx) = rightmost_unmatched_open_paren(&tokens[..end]) {
            let new_end = if open_idx > 0
                && matches!(tokens[open_idx - 1].kind, TokenKind::Ident(_))
            {
                open_idx - 1
            } else {
                open_idx
            };
            end = new_end;
            changed = true;
            continue;
        }

        let last = &tokens[end - 1].kind;
        match last {
            TokenKind::Pipe => {
                end -= 1;
                changed = true;
            }
            TokenKind::Comma | TokenKind::Semi => {
                end = strip_trailing_operand(tokens, end - 1);
                changed = true;
            }
            TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Le
            | TokenKind::Gt
            | TokenKind::Ge => {
                end = strip_trailing_operand(tokens, end - 1);
                changed = true;
            }
            TokenKind::Assign => {
                end -= 1;
                if end > 0 {
                    if let TokenKind::Ident(s) = &tokens[end - 1].kind {
                        if !grammar::is_keyword(s) {
                            end -= 1;
                        }
                    }
                }
                if end > 0 {
                    if let TokenKind::Ident(s) = &tokens[end - 1].kind {
                        if s == kw::LET {
                            end -= 1;
                        }
                    }
                }
                changed = true;
            }
            TokenKind::Ident(s) => {
                let s = s.as_str();
                if s == kw::AND || s == kw::OR {
                    end = strip_trailing_operand(tokens, end - 1);
                    changed = true;
                } else if s == kw::NOT {
                    end -= 1;
                    changed = true;
                } else if s == kw::BY {
                    end = strip_trailing_operand(tokens, end - 1);
                    if end > 0 {
                        if let TokenKind::Ident(k) = &tokens[end - 1].kind {
                            if k == kw::COLLECT || k == kw::ORDER {
                                end -= 1;
                            }
                        }
                    }
                    changed = true;
                } else if is_pattern_comparison(s) {
                    end = strip_trailing_operand(tokens, end - 1);
                    changed = true;
                } else if is_clause_with_body(s) {
                    end -= 1;
                    changed = true;
                }
                // Otherwise (literal keyword, reducer, or plain ident): stop.
            }
            _ => {}
        }
    }

    // `end` is the index of the first non-kept token (or tokens.len()
    // if all were kept). Slice pre_partial up to that token's char
    // position (the trailing dot's position when end == tokens.len()-1).
    let char_end = if end >= tokens.len() {
        pre_partial.chars().count()
    } else {
        tokens[end].position
    };
    let byte_end = char_index_to_byte(&pre_partial, char_end);
    let slice = pre_partial[..byte_end].trim();
    if slice.is_empty() {
        ".".to_string()
    } else {
        slice.to_string()
    }
}

/// Walks tokens[..end] backward and returns the token index where the
/// trailing operand starts. Operand boundaries are top-level (depth 0)
/// pipes, commas, semicolons, comparison ops, assignment, and clause
/// keywords — anything that would syntactically delimit an operand.
fn strip_trailing_operand(tokens: &[Token], end: usize) -> usize {
    let mut i = end as i64 - 1;
    let mut depth: i32 = 0;
    while i >= 0 {
        let t = &tokens[i as usize].kind;
        match t {
            TokenKind::RParen => depth += 1,
            TokenKind::LParen => {
                if depth > 0 {
                    depth -= 1;
                } else {
                    return (i + 1) as usize;
                }
            }
            _ if depth == 0 && is_operand_boundary(t) => {
                return (i + 1) as usize;
            }
            _ => {}
        }
        i -= 1;
    }
    0
}

fn is_operand_boundary(t: &TokenKind) -> bool {
    use TokenKind::*;
    match t {
        Pipe | Comma | Semi | Eq | Ne | Lt | Le | Gt | Ge | Assign => true,
        Ident(s) => matches!(
            s.as_str(),
            "and" | "or" | "not"
                | "where" | "select" | "aggregate" | "collect" | "order"
                | "limit" | "by"
                | "from" | "join" | "on" | "as"
                | "let" | "distinct" | "fields" | "in"
                | "matches" | "starts_with" | "ends_with" | "contains"
        ),
        _ => false,
    }
}

/// Returns the index of the rightmost unmatched `(` in `tokens`, if
/// any. Walks backward tracking paren depth.
fn rightmost_unmatched_open_paren(tokens: &[Token]) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut i = tokens.len() as i64 - 1;
    while i >= 0 {
        match tokens[i as usize].kind {
            TokenKind::RParen => depth += 1,
            TokenKind::LParen => {
                if depth > 0 {
                    depth -= 1;
                } else {
                    return Some(i as usize);
                }
            }
            _ => {}
        }
        i -= 1;
    }
    None
}

/// Forgiving tokenization. Falls back by truncating at the lex error
/// position and retrying, so partial input (e.g. an unterminated
/// identifier sequence we don't care about) still yields a usable
/// prefix.
fn lenient_tokenize(s: &str) -> Vec<Token> {
    let mut text = s.to_string();
    loop {
        match tokenize(&text) {
            Ok(mut toks) => {
                if matches!(toks.last().map(|t| &t.kind), Some(TokenKind::Eof)) {
                    toks.pop();
                }
                return toks;
            }
            Err(e) => {
                let chars: Vec<char> = text.chars().collect();
                if e.position >= chars.len() || e.position == 0 {
                    return Vec::new();
                }
                text = chars[..e.position].iter().collect();
            }
        }
    }
}

/// True when `s` ends inside an unterminated string literal — there's
/// an open `"` with no matching close, ignoring escaped quotes.
fn inside_open_string(s: &str) -> bool {
    let mut in_str = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if !in_str {
            if c == '"' {
                in_str = true;
            }
        } else if c == '\\' {
            chars.next();
        } else if c == '"' {
            in_str = false;
        }
    }
    in_str
}

/// Converts a char index (UTF-32 codepoint index) into a byte offset
/// within `s`. Mirrors `String::byte_offset_of_nth_char` if it existed.
fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    let mut count = 0;
    for (b, _) in s.char_indices() {
        if count == char_idx {
            return b;
        }
        count += 1;
    }
    s.len()
}

/// Truncates `source` at a UTF-16 offset and returns the resulting
/// (valid) UTF-8 string. The UI sends cursor positions as UTF-16
/// offsets (JavaScript string indexing); this mirrors `source.slice(0,
/// cursor)` on that side.
fn truncate_at_utf16(source: &str, cursor_utf16: u32) -> String {
    let mut units = 0u32;
    let mut byte_end = source.len();
    for (byte_idx, ch) in source.char_indices() {
        if units >= cursor_utf16 {
            byte_end = byte_idx;
            break;
        }
        units += if (ch as u32) > 0xFFFF { 2 } else { 1 };
    }
    if units < cursor_utf16 {
        byte_end = source.len();
    }
    source[..byte_end].to_string()
}

/// Mode discriminant string for FFI.
pub fn mode_str(m: &CompletionMode) -> &'static str {
    match m {
        CompletionMode::FieldAccess => "fieldAccess",
        CompletionMode::ValueStart => "valueStart",
        CompletionMode::AfterExpression => "afterExpression",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_at_end(src: &str) -> Option<CompletionContext> {
        let cursor = src.encode_utf16().count() as u32;
        classify(src, cursor)
    }

    #[test]
    fn empty_query_is_value_start() {
        let c = classify_at_end("").unwrap();
        assert_eq!(c.mode, CompletionMode::ValueStart);
        assert_eq!(c.partial, "");
    }

    #[test]
    fn after_from_keyword_is_value_start() {
        let c = classify_at_end("from ").unwrap();
        assert_eq!(c.mode, CompletionMode::ValueStart);
    }

    #[test]
    fn dot_triggers_field_access_root() {
        let c = classify_at_end(".").unwrap();
        assert_eq!(c.mode, CompletionMode::FieldAccess);
        assert_eq!(c.context_query.as_deref(), Some("."));
    }

    #[test]
    fn dot_after_path_resolves_against_path() {
        let c = classify_at_end(".users[].").unwrap();
        assert_eq!(c.mode, CompletionMode::FieldAccess);
        assert_eq!(c.context_query.as_deref(), Some(".users[]"));
    }

    #[test]
    fn dot_in_where_clause_resolves_against_source() {
        let c = classify_at_end("from .users as u where .").unwrap();
        assert_eq!(c.mode, CompletionMode::FieldAccess);
        let ctx = c.context_query.unwrap();
        assert!(ctx.contains(".users"), "got context: {}", ctx);
    }

    #[test]
    fn after_complete_path_is_after_expression() {
        let c = classify_at_end(".users ").unwrap();
        assert_eq!(c.mode, CompletionMode::AfterExpression);
    }

    #[test]
    fn mid_partial_returns_partial() {
        let c = classify_at_end("fr").unwrap();
        assert_eq!(c.mode, CompletionMode::ValueStart);
        assert_eq!(c.partial, "fr");
    }

    #[test]
    fn after_by_keyword_is_value_start() {
        let c = classify_at_end("count by ").unwrap();
        assert_eq!(c.mode, CompletionMode::ValueStart);
    }

    #[test]
    fn after_compare_op_strips_to_input() {
        let c = classify_at_end("from .users as u where u.role == ").unwrap();
        assert_eq!(c.mode, CompletionMode::ValueStart);
    }

    #[test]
    fn keyword_partial_does_not_match_inside_word() {
        let c = classify_at_end("goodby").unwrap();
        assert_eq!(c.partial, "goodby");
        assert_eq!(c.mode, CompletionMode::ValueStart);
    }

    #[test]
    fn dot_inside_string_returns_none() {
        let c = classify_at_end("from .x as x where x.role == \"adm.");
        assert!(c.is_none());
    }

    #[test]
    fn mid_number_returns_none() {
        let c = classify_at_end("limit 4");
        assert!(c.is_none());
    }

    #[test]
    fn collect_by_then_dot() {
        let c = classify_at_end("from .users as u collect by .").unwrap();
        assert_eq!(c.mode, CompletionMode::FieldAccess);
    }

    #[test]
    fn after_and_keyword_value_start() {
        let c = classify_at_end("from .x as x where x.role == \"a\" and ").unwrap();
        assert_eq!(c.mode, CompletionMode::ValueStart);
    }

    #[test]
    fn aggregate_block_by_dot() {
        let c = classify_at_end("from .x as x aggregate { n: count() } by .").unwrap();
        assert_eq!(c.mode, CompletionMode::FieldAccess);
    }
}
