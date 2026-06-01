use super::grammar::{self, TokenCategory};
use super::QueryError;

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Dot,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Semi,
    Pipe,
    Question,
    /// `??` — coalesce / fallback. Recognised by the surface parser
    /// inside an aggregate-block reduction (`sum X ?? DEFAULT`) to
    /// override the engine's "empty reducer → null" default.
    QuestionQuestion,
    LParen,
    RParen,
    /// Single-level wildcard. `.*` after a node emits each immediate
    /// child (object value or array element) — same shape as `[]`.
    /// Also doubles as the multiplication operator in arithmetic
    /// contexts; the parser decides which by position.
    Star,
    /// Deep wildcard. `.**` walks the entire subtree and lets a
    /// following `.field` match anywhere in it.
    StarStar,
    Ident(String),
    /// `$name` query parameter reference.
    Param(String),
    Str(String),
    Number(f64),
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Bare `=`. Used by the surface parser for `fields NAME = {…}` and
    /// `let NAME = expr`.
    Assign,
    /// Binary `+`. Arithmetic only — no unary plus form is recognised.
    Plus,
    /// Binary `-`. Emitted when the prior token can end a value
    /// expression (so subtraction is natural); otherwise `-` continues
    /// to start a negative-number literal. Unary minus on a non-literal
    /// (e.g. `-.x`) is parsed at the primary rung.
    Minus,
    /// Binary `/`. Arithmetic only.
    Slash,
    Eof,
}

impl TokenKind {
    pub fn description(&self) -> String {
        match self {
            TokenKind::Dot => ".".into(),
            TokenKind::LBrack => "[".into(),
            TokenKind::RBrack => "]".into(),
            TokenKind::LBrace => "{".into(),
            TokenKind::RBrace => "}".into(),
            TokenKind::Colon => ":".into(),
            TokenKind::Comma => ",".into(),
            TokenKind::Semi => ";".into(),
            TokenKind::Pipe => "|".into(),
            TokenKind::Question => "?".into(),
            TokenKind::QuestionQuestion => "??".into(),
            TokenKind::LParen => "(".into(),
            TokenKind::RParen => ")".into(),
            TokenKind::Star => "*".into(),
            TokenKind::StarStar => "**".into(),
            TokenKind::Ident(s) => format!("‘{}’", s),
            TokenKind::Param(s) => format!("‘${}’", s),
            TokenKind::Str(s) => format!("string “{}”", s),
            TokenKind::Number(n) => format!("number {}", n),
            TokenKind::Eq => "==".into(),
            TokenKind::Ne => "!=".into(),
            TokenKind::Lt => "<".into(),
            TokenKind::Le => "<=".into(),
            TokenKind::Gt => ">".into(),
            TokenKind::Ge => ">=".into(),
            TokenKind::Assign => "=".into(),
            TokenKind::Plus => "+".into(),
            TokenKind::Minus => "-".into(),
            TokenKind::Slash => "/".into(),
            TokenKind::Eof => "end of input".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub position: usize,
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, QueryError> {
    let chars: Vec<char> = source.chars().collect();
    let mut lexer = Lexer { chars, pos: 0 };
    lexer.run()
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn run(&mut self) -> Result<Vec<Token>, QueryError> {
        let mut tokens = Vec::new();
        while self.pos < self.chars.len() {
            self.skip_ws();
            if self.pos >= self.chars.len() {
                break;
            }
            let start = self.pos;
            let ch = self.chars[self.pos];
            let kind = match ch {
                '.' => { self.pos += 1; TokenKind::Dot }
                '[' => { self.pos += 1; TokenKind::LBrack }
                ']' => { self.pos += 1; TokenKind::RBrack }
                '{' => { self.pos += 1; TokenKind::LBrace }
                '}' => { self.pos += 1; TokenKind::RBrace }
                ':' => { self.pos += 1; TokenKind::Colon }
                ',' => { self.pos += 1; TokenKind::Comma }
                ';' => { self.pos += 1; TokenKind::Semi }
                '|' => { self.pos += 1; TokenKind::Pipe }
                '?' => {
                    self.pos += 1;
                    if self.peek() == Some('?') {
                        self.pos += 1;
                        TokenKind::QuestionQuestion
                    } else {
                        TokenKind::Question
                    }
                }
                '(' => { self.pos += 1; TokenKind::LParen }
                ')' => { self.pos += 1; TokenKind::RParen }
                '*' => {
                    self.pos += 1;
                    if self.peek() == Some('*') {
                        self.pos += 1;
                        TokenKind::StarStar
                    } else {
                        TokenKind::Star
                    }
                }
                '=' => {
                    self.pos += 1;
                    if self.peek() == Some('=') {
                        self.pos += 1;
                        TokenKind::Eq
                    } else {
                        TokenKind::Assign
                    }
                }
                '!' => self.match_double('=', TokenKind::Ne, "expected '!=' (got bare '!')")?,
                '<' => {
                    self.pos += 1;
                    if self.peek() == Some('=') { self.pos += 1; TokenKind::Le } else { TokenKind::Lt }
                }
                '>' => {
                    self.pos += 1;
                    if self.peek() == Some('=') { self.pos += 1; TokenKind::Ge } else { TokenKind::Gt }
                }
                '"' => self.read_string(start)?,
                '+' => { self.pos += 1; TokenKind::Plus }
                '/' => { self.pos += 1; TokenKind::Slash }
                '-' => {
                    // Contextual: when the previous emitted token can
                    // end a value expression (Ident, Number, Str, `)`,
                    // `]`, `}`, splat) the next `-` is binary subtraction.
                    // Otherwise (`where -5`, `[-1]`, start of input) it
                    // continues to start a negative-number literal — but
                    // only when the next char is a digit. A `-` followed
                    // by a non-digit at a value-start position is unary
                    // minus, handled in the parser.
                    if prev_can_end_value(tokens.last()) {
                        self.pos += 1;
                        TokenKind::Minus
                    } else if matches!(self.chars.get(self.pos + 1), Some(c) if c.is_ascii_digit()) {
                        self.read_number(start)?
                    } else {
                        self.pos += 1;
                        TokenKind::Minus
                    }
                }
                '$' => self.read_param(start)?,
                c if c.is_ascii_digit() => self.read_number(start)?,
                c if c.is_alphabetic() || c == '_' => self.read_ident(),
                _ => return Err(QueryError::new(start, format!("unexpected character ‘{}’", ch))),
            };
            tokens.push(Token { kind, position: start });
        }
        tokens.push(Token { kind: TokenKind::Eof, position: self.pos });
        Ok(tokens)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
}

/// True if the given token can syntactically end a value expression —
/// the lexer uses this to disambiguate binary `-` from a leading-`-`
/// negative-number literal. Identifiers (paths, names), literals (number,
/// string), closing brackets, and splat tokens count. Keyword identifiers
/// like `where`/`sum` are handled by the parser via the `Ident` arm —
/// distinguishing them in the lexer would require duplicating the keyword
/// list; the contextual rule with the next-char fallback covers the cases
/// where it matters.
fn prev_can_end_value(tok: Option<&Token>) -> bool {
    match tok.map(|t| &t.kind) {
        Some(TokenKind::Ident(_))
        | Some(TokenKind::Param(_))
        | Some(TokenKind::Number(_))
        | Some(TokenKind::Str(_))
        | Some(TokenKind::RParen)
        | Some(TokenKind::RBrack)
        | Some(TokenKind::RBrace)
        | Some(TokenKind::Star)
        | Some(TokenKind::StarStar) => true,
        _ => false,
    }
}

impl Lexer {

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn match_double(
        &mut self,
        next: char,
        token: TokenKind,
        err: &str,
    ) -> Result<TokenKind, QueryError> {
        let start = self.pos;
        self.pos += 1;
        if self.peek() == Some(next) {
            self.pos += 1;
            Ok(token)
        } else {
            Err(QueryError::new(start, err.into()))
        }
    }

    fn read_ident(&mut self) -> TokenKind {
        let start = self.pos;
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if c.is_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        TokenKind::Ident(self.chars[start..self.pos].iter().collect())
    }

    fn read_param(&mut self, start: usize) -> Result<TokenKind, QueryError> {
        self.pos += 1; // consume `$`
        let name_start = self.pos;
        if !matches!(self.peek(), Some(c) if c.is_alphabetic() || c == '_') {
            return Err(QueryError::new(
                start,
                "expected a parameter name after ‘$’".into(),
            ));
        }
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        Ok(TokenKind::Param(self.chars[name_start..self.pos].iter().collect()))
    }

    fn read_number(&mut self, start: usize) -> Result<TokenKind, QueryError> {
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            return Err(QueryError::new(start, "expected digit".into()));
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let raw: String = self.chars[start..self.pos].iter().collect();
        match raw.parse::<f64>() {
            Ok(n) => Ok(TokenKind::Number(n)),
            Err(_) => Err(QueryError::new(start, format!("invalid number ‘{}’", raw))),
        }
    }

    fn read_string(&mut self, start: usize) -> Result<TokenKind, QueryError> {
        self.pos += 1; // opening "
        let mut s = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return Err(QueryError::new(start, "unterminated string".into())),
            };
            if c == '"' {
                self.pos += 1;
                return Ok(TokenKind::Str(s));
            }
            if c == '\\' {
                self.pos += 1;
                let esc = self
                    .peek()
                    .ok_or_else(|| QueryError::new(self.pos, "unterminated escape".into()))?;
                match esc {
                    '"' => { s.push('"'); self.pos += 1; }
                    '\\' => { s.push('\\'); self.pos += 1; }
                    '/' => { s.push('/'); self.pos += 1; }
                    'b' => { s.push('\u{08}'); self.pos += 1; }
                    'f' => { s.push('\u{0C}'); self.pos += 1; }
                    'n' => { s.push('\n'); self.pos += 1; }
                    'r' => { s.push('\r'); self.pos += 1; }
                    't' => { s.push('\t'); self.pos += 1; }
                    'u' => {
                        self.pos += 1;
                        let code = self.read_hex4()?;
                        if let Some(ch) = char::from_u32(code) {
                            s.push(ch);
                        } else {
                            s.push('\u{FFFD}');
                        }
                    }
                    other => {
                        return Err(QueryError::new(
                            self.pos,
                            format!("invalid escape ‘\\{}’", other),
                        ));
                    }
                }
                continue;
            }
            s.push(c);
            self.pos += 1;
        }
    }

    fn read_hex4(&mut self) -> Result<u32, QueryError> {
        let mut code = 0u32;
        for _ in 0..4 {
            let c = self
                .peek()
                .ok_or_else(|| QueryError::new(self.pos, "incomplete \\u escape".into()))?;
            let v = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'a'..='f' => c as u32 - 'a' as u32 + 10,
                'A'..='F' => c as u32 - 'A' as u32 + 10,
                _ => return Err(QueryError::new(self.pos, "invalid hex digit".into())),
            };
            code = code * 16 + v;
            self.pos += 1;
        }
        Ok(code)
    }
}

// ============================================================================
// UI tokenizer
//
// Forgiving scanner used by the Swift highlighter via FFI. Returns spans
// in UTF-16 code units (offsets compatible with NSTextStorage / NSRange).
// Never fails — emits `Error` tokens for unrecognised bytes, malformed
// numbers, and unterminated strings so the entire query string is
// covered without losing characters.
//
// This is intentionally separate from the strict `tokenize` used by the
// parser: the strict tokenizer must reject malformed input so parse
// errors point at the offending character; the UI tokenizer must keep
// painting partial syntax as the user types.
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct UiToken {
    pub category: TokenCategory,
    /// Offset in UTF-16 code units from the start of the source.
    pub offset: u32,
    /// Length in UTF-16 code units.
    pub length: u32,
}

pub fn tokenize_for_ui(source: &str) -> Vec<UiToken> {
    let chars: Vec<char> = source.chars().collect();
    let mut scanner = UiScanner {
        chars,
        pos: 0,
        utf16_pos: 0,
    };
    scanner.run()
}

struct UiScanner {
    chars: Vec<char>,
    pos: usize,
    utf16_pos: u32,
}

impl UiScanner {
    fn run(&mut self) -> Vec<UiToken> {
        let mut out = Vec::new();
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];

            // Whitespace — emit no token, just advance.
            if c.is_whitespace() {
                self.advance_one();
                continue;
            }

            let start_utf16 = self.utf16_pos;

            // `#` to end of line — comment.
            if c == '#' {
                while self.pos < self.chars.len() && self.chars[self.pos] != '\n' {
                    self.advance_one();
                }
                out.push(self.token(TokenCategory::Comment, start_utf16));
                continue;
            }

            // String literal. Forgiving: an unterminated string runs to
            // EOF and is emitted as a single `String` token.
            if c == '"' {
                self.advance_one();
                while self.pos < self.chars.len() {
                    let ch = self.chars[self.pos];
                    if ch == '\\' {
                        self.advance_one();
                        if self.pos < self.chars.len() {
                            self.advance_one();
                        }
                        continue;
                    }
                    if ch == '"' {
                        self.advance_one();
                        break;
                    }
                    self.advance_one();
                }
                out.push(self.token(TokenCategory::String, start_utf16));
                continue;
            }

            // Number. Match the strict lexer's shape (optional sign,
            // digits, optional decimal, optional exponent) but never
            // fail on invalid trailing chars — the parser will catch it.
            if c.is_ascii_digit()
                || (c == '-'
                    && self
                        .chars
                        .get(self.pos + 1)
                        .map(|n| n.is_ascii_digit())
                        .unwrap_or(false))
            {
                if c == '-' {
                    self.advance_one();
                }
                while self
                    .chars
                    .get(self.pos)
                    .map(|n| n.is_ascii_digit())
                    .unwrap_or(false)
                {
                    self.advance_one();
                }
                if self.chars.get(self.pos) == Some(&'.') {
                    self.advance_one();
                    while self
                        .chars
                        .get(self.pos)
                        .map(|n| n.is_ascii_digit())
                        .unwrap_or(false)
                    {
                        self.advance_one();
                    }
                }
                if matches!(self.chars.get(self.pos), Some('e') | Some('E')) {
                    self.advance_one();
                    if matches!(self.chars.get(self.pos), Some('+') | Some('-')) {
                        self.advance_one();
                    }
                    while self
                        .chars
                        .get(self.pos)
                        .map(|n| n.is_ascii_digit())
                        .unwrap_or(false)
                    {
                        self.advance_one();
                    }
                }
                out.push(self.token(TokenCategory::Number, start_utf16));
                continue;
            }

            // Identifier — letters, digits (after first), underscore.
            // Look up against the manifest to classify as
            // keyword / reducer / literal / identifier.
            if c.is_alphabetic() || c == '_' {
                let ident_start = self.pos;
                self.advance_one();
                while self
                    .chars
                    .get(self.pos)
                    .map(|n| n.is_alphanumeric() || *n == '_')
                    .unwrap_or(false)
                {
                    self.advance_one();
                }
                let word: String = self.chars[ident_start..self.pos].iter().collect();
                let category = grammar::identifier_category(&word);
                out.push(self.token(category, start_utf16));
                continue;
            }

            // Query parameter `$name`. A lone `$` (or `$` followed by a
            // non-identifier char) still emits an Identifier-categorised
            // token so the highlighter keeps painting as the user types.
            if c == '$' {
                self.advance_one();
                while self
                    .chars
                    .get(self.pos)
                    .map(|n| n.is_alphanumeric() || *n == '_')
                    .unwrap_or(false)
                {
                    self.advance_one();
                }
                out.push(self.token(TokenCategory::Identifier, start_utf16));
                continue;
            }

            // Splat — `**` first so the longer match wins.
            if c == '*' {
                self.advance_one();
                if self.chars.get(self.pos) == Some(&'*') {
                    self.advance_one();
                }
                out.push(self.token(TokenCategory::Splat, start_utf16));
                continue;
            }

            // Multi-char operators. `=` may be followed by `=` (Eq).
            if c == '=' {
                self.advance_one();
                if self.chars.get(self.pos) == Some(&'=') {
                    self.advance_one();
                }
                out.push(self.token(TokenCategory::Operator, start_utf16));
                continue;
            }
            if c == '!' && self.chars.get(self.pos + 1) == Some(&'=') {
                self.advance_one();
                self.advance_one();
                out.push(self.token(TokenCategory::Operator, start_utf16));
                continue;
            }
            if c == '<' || c == '>' {
                self.advance_one();
                if self.chars.get(self.pos) == Some(&'=') {
                    self.advance_one();
                }
                out.push(self.token(TokenCategory::Operator, start_utf16));
                continue;
            }

            // Arithmetic single-char operators. `-` was already handled
            // above as part of the number-literal path when it leads a
            // digit; anything that lands here is binary or unary minus.
            if c == '+' || c == '-' || c == '/' {
                self.advance_one();
                out.push(self.token(TokenCategory::Operator, start_utf16));
                continue;
            }

            // Single-char punctuation.
            if matches!(
                c,
                '.' | ',' | ';' | ':' | '|' | '?' | '(' | ')' | '[' | ']' | '{' | '}'
            ) {
                self.advance_one();
                out.push(self.token(TokenCategory::Punctuation, start_utf16));
                continue;
            }

            // Anything we don't recognise — emit as Error so the UI can
            // colour it (or just leave it base-coloured) and keep going.
            self.advance_one();
            out.push(self.token(TokenCategory::Error, start_utf16));
        }
        out
    }

    fn advance_one(&mut self) {
        let c = self.chars[self.pos];
        self.pos += 1;
        // Surrogate pairs (chars outside the BMP) take 2 UTF-16 units.
        self.utf16_pos += if (c as u32) > 0xFFFF { 2 } else { 1 };
    }

    fn token(&self, category: TokenCategory, start_utf16: u32) -> UiToken {
        UiToken {
            category,
            offset: start_utf16,
            length: self.utf16_pos - start_utf16,
        }
    }
}
