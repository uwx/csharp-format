//! A lossless, error-tolerant lexer for C# 7 through C# 15 / .NET 11.
//!
//! The lexer never rewrites source text. Every byte of the input is accounted for either as
//! whitespace in a [`Gap`], as a token, or as an explicit lexical error. This makes the
//! formatter's core invariant expressible as: *the formatter only ever inserts or removes
//! whitespace between tokens*.
//!
//! The scanner understands the constructs that matter for finding token boundaries:
//!
//! * verbatim, interpolated, verbatim-interpolated, raw and raw-interpolated string literals
//!   (including nested string literals inside interpolation holes),
//! * UTF-8 string literals (`"...":u8`),
//! * char literals and all escape forms,
//! * preprocessor directives as single line tokens,
//! * line and block comments as first-class tokens,
//! * every C# punctuation token, longest match first.

use std::fmt;

/// Broad lexical category. Contextual keywords are classified as [`TokenKind::Keyword`]
/// because spacing rules only care about the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Ident,
    Keyword,
    Number,
    String,
    Char,
    Punct,
    LineComment,
    BlockComment,
    Directive,
    Unknown,
}

impl TokenKind {
    #[must_use]
    pub fn is_comment(self) -> bool {
        matches!(self, Self::LineComment | Self::BlockComment)
    }

    #[must_use]
    pub fn is_literal(self) -> bool {
        matches!(self, Self::Number | Self::String | Self::Char)
    }
}

/// Whitespace between two tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gap {
    /// Number of line terminators (`\n`, `\r\n` and `\r` each count as one).
    pub newlines: u32,
    /// Horizontal spaces preceding the token on its own line (or since the previous token
    /// on the same line).
    pub spaces: u32,
    /// Tabs preceding the token on its own line.
    pub tabs: u32,
}

impl Gap {
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.newlines == 0 && self.spaces == 0 && self.tabs == 0
    }

    #[must_use]
    pub fn same_line(self) -> bool {
        self.newlines == 0
    }

    fn take(&mut self, other: Gap) {
        self.newlines += other.newlines;
        if other.newlines == 0 {
            self.spaces += other.spaces;
            self.tabs += other.tabs;
        } else {
            self.spaces = other.spaces;
            self.tabs = other.tabs;
        }
    }
}

/// A single lexical token. Offsets are byte offsets into the source.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    /// 1-based line number of the token start.
    pub line: u32,
    /// Whitespace preceding this token.
    pub gap: Gap,
    /// Syntax metadata filled in by [`crate::syntax::Syntax::annotate`].
    pub ast: crate::syntax::AstInfo,
}

impl Token {
    #[must_use]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start..self.end]
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub fn is_trivia(&self) -> bool {
        self.kind.is_comment() || self.kind == TokenKind::Directive
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub offset: usize,
    pub line: u32,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Result of lexing a document.
#[derive(Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
    /// True when the source started with a UTF-8 BOM.
    pub has_bom: bool,
}

/// Lex a complete C# document.
#[must_use]
pub fn lex(source: &str) -> Lexed {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    errors: Vec<LexError>,
    tokens: Vec<Token>,
    has_bom: bool,
}

const KEYWORDS: &[&str] = &[
    // Reserved keywords.
    "abstract",
    "as",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    // Contextual keywords. Classified as keywords so that spacing heuristics see them as words.
    "add",
    "alias",
    "and",
    "as",
    "ascending",
    "async",
    "await",
    "by",
    "descending",
    "dynamic",
    "equals",
    "file",
    "from",
    "get",
    "global",
    "group",
    "init",
    "into",
    "join",
    "let",
    "managed",
    "nameof",
    "nint",
    "not",
    "notnull",
    "nuint",
    "on",
    "or",
    "orderby",
    "partial",
    "record",
    "remove",
    "required",
    "scoped",
    "select",
    "set",
    "unmanaged",
    "value",
    "var",
    "when",
    "where",
    "with",
    "yield",
    "extension",
    "field",
    "allows",
];

fn is_keyword(text: &str) -> bool {
    KEYWORDS.contains(&text)
}

const PUNCTUATION: &[&str] = &[
    ">>>=", "<<=", ">>=", "??=", ">>>", "=>", "==", "!=", "<=", ">=", "++", "--", "&&", "||", "->",
    "::", "?.", "?[", "??", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "..", ">",
    "<", "=", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", ";", ",", ".", "(", ")",
    "[", "]", "{", "}", "@", "#", "$", "\\",
];

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        let mut pos = 0;
        let has_bom = src.starts_with('\u{feff}');
        if has_bom {
            pos = '\u{feff}'.len_utf8();
        }
        Self {
            src,
            bytes: src.as_bytes(),
            pos,
            line: 1,
            errors: Vec::new(),
            tokens: Vec::with_capacity(src.len() / 4 + 16),
            has_bom,
        }
    }

    fn run(mut self) -> Lexed {
        let mut gap = Gap::default();
        loop {
            gap.take(self.skip_whitespace());
            if self.pos >= self.src.len() {
                break;
            }
            let start = self.pos;
            let line = self.line;
            self.scan_token();
            if self.pos == start {
                // Defensive: never stall, even for truly unknown input.
                let ch = self.peek_char().unwrap_or('\0');
                self.advance_char();
                self.errors.push(LexError {
                    message: format!("unknown character {ch:?}"),
                    offset: start,
                    line,
                });
            }
            let kind = self.classify(start);
            self.tokens.push(Token {
                kind,
                start,
                end: self.pos,
                line,
                gap: std::mem::take(&mut gap),
                ast: crate::syntax::AstInfo::default(),
            });
        }
        Lexed {
            tokens: self.tokens,
            errors: self.errors,
            has_bom: self.has_bom,
        }
    }

    fn classify(&self, start: usize) -> TokenKind {
        let text = &self.src[start..self.pos];
        if text.starts_with("//") {
            return TokenKind::LineComment;
        }
        if text.starts_with("/*") {
            return TokenKind::BlockComment;
        }
        if text.starts_with('#') {
            return TokenKind::Directive;
        }
        let first = self.bytes[start];
        match first {
            b'"' | b'\'' => {
                if first == b'\'' {
                    TokenKind::Char
                } else {
                    TokenKind::String
                }
            }
            b'@' if text.len() > 1
                && (text.as_bytes()[1] == b'"' || text.as_bytes()[1] == b'$') =>
            {
                TokenKind::String
            }
            b'$' => TokenKind::String,
            b'0'..=b'9' => TokenKind::Number,
            b'.' if text.len() > 1 && text.as_bytes()[1].is_ascii_digit() => TokenKind::Number,
            b'_' | b'@' => {
                if text.starts_with('@') {
                    TokenKind::Ident
                } else if is_keyword(text) {
                    TokenKind::Keyword
                } else {
                    TokenKind::Ident
                }
            }
            _ if first.is_ascii_alphabetic() => {
                if is_keyword(text) {
                    TokenKind::Keyword
                } else {
                    TokenKind::Ident
                }
            }
            _ if first >= 0x80 => {
                if is_keyword(text) {
                    TokenKind::Keyword
                } else {
                    TokenKind::Ident
                }
            }
            _ => {
                if text.len() == 1 && PUNCTUATION.contains(&text) {
                    TokenKind::Punct
                } else if PUNCTUATION.contains(&text) {
                    TokenKind::Punct
                } else {
                    TokenKind::Unknown
                }
            }
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_byte(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s)
    }

    fn advance_char(&mut self) {
        if let Some(ch) = self.peek_char() {
            self.pos += ch.len_utf8();
            if ch == '\n' {
                self.line += 1;
            }
        }
    }

    fn advance_to(&mut self, new_pos: usize) {
        debug_assert!(new_pos <= self.src.len());
        self.line += self.src[self.pos..new_pos].matches('\n').count() as u32;
        self.pos = new_pos;
    }

    fn skip_whitespace(&mut self) -> Gap {
        let mut gap = Gap::default();
        while let Some(ch) = self.peek_char() {
            match ch {
                ' ' => {
                    gap.spaces += 1;
                    self.pos += 1;
                }
                '\t' => {
                    gap.tabs += 1;
                    self.pos += 1;
                }
                '\n' => {
                    gap.newlines += 1;
                    gap.spaces = 0;
                    gap.tabs = 0;
                    self.pos += 1;
                    self.line += 1;
                }
                '\r' => {
                    gap.newlines += 1;
                    gap.spaces = 0;
                    gap.tabs = 0;
                    self.pos += 1;
                    if self.peek_char() == Some('\n') {
                        self.pos += 1;
                    }
                    self.line += 1;
                }
                '\u{feff}' => {
                    self.pos += ch.len_utf8();
                }
                c if c.is_whitespace() => {
                    gap.spaces += 1;
                    self.pos += c.len_utf8();
                }
                _ => break,
            }
        }
        gap
    }

    fn scan_token(&mut self) {
        let ch = self.peek_char().unwrap_or('\0');
        let at_line_start = self
            .tokens
            .last()
            .is_none_or(|t| t.end <= self.line_start_offset());
        match ch {
            '/' if self.peek_byte(1) == Some(b'/') => self.scan_line_comment(),
            '/' if self.peek_byte(1) == Some(b'*') => self.scan_block_comment(),
            '#' if at_line_start => self.scan_directive(),
            '\'' => self.scan_char(),
            '"' => {
                if self.quote_run() >= 3 {
                    self.scan_string_literal();
                } else {
                    self.scan_string_literal();
                }
            }
            '@' if self.peek_byte(1) == Some(b'"') => self.scan_string_literal(),
            '@' if self.peek_byte(1) == Some(b'$') && self.peek_byte(2) == Some(b'"') => {
                self.scan_string_literal();
            }
            '@' if self.peek_byte(1) == Some(b'$') && self.peek_byte(2) == Some(b'$') => {
                self.scan_string_literal();
            }
            '$' => self.scan_string_literal(),
            c if c.is_ascii_digit() => self.scan_number(),
            '.' if self.peek_byte(1).is_some_and(|b| b.is_ascii_digit()) => self.scan_number(),
            c if c == '_' || c == '@' || unicode_ident::is_xid_start(c) => self.scan_identifier(),
            _ => self.scan_punctuation(),
        }
    }

    fn line_start_offset(&self) -> usize {
        match self.src[..self.pos].rfind('\n') {
            Some(idx) => idx + 1,
            None => 0,
        }
    }

    fn scan_line_comment(&mut self) {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if ch == '\n' || ch == '\r' {
                break;
            }
            self.advance_char();
        }
        if self.pos == start {
            self.advance_char();
        }
    }

    fn scan_block_comment(&mut self) {
        let start = self.pos;
        self.advance_to(self.pos + 2);
        while self.pos < self.src.len() {
            if self.starts_with("*/") {
                self.advance_to(self.pos + 2);
                return;
            }
            self.advance_char();
        }
        self.errors.push(LexError {
            message: "unterminated block comment".into(),
            offset: start,
            line: self.line,
        });
    }

    fn scan_directive(&mut self) {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if ch == '\n' || ch == '\r' {
                break;
            }
            if ch == '\\' && self.peek_byte(1) == Some(b'\n') {
                self.advance_to(self.pos + 2);
                continue;
            }
            self.advance_char();
        }
        if self.pos == start {
            self.advance_char();
        }
    }

    fn scan_identifier(&mut self) {
        if self.peek_char() == Some('@') {
            self.pos += 1;
        }
        if let Some(ch) = self.peek_char()
            && (ch == '_' || unicode_ident::is_xid_start(ch))
        {
            self.advance_char();
        }
        while let Some(ch) = self.peek_char() {
            if ch == '_' || unicode_ident::is_xid_continue(ch) {
                self.advance_char();
            } else if ch == '\\' && self.peek_byte(1) == Some(b'u') {
                // Unicode escape inside an identifier.
                self.advance_to(self.pos + 2);
                for _ in 0..4 {
                    if self.peek_byte(0).is_some_and(|b| b.is_ascii_hexdigit()) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn scan_number(&mut self) {
        let bytes = self.bytes;
        let len = bytes.len();
        let mut i = self.pos;
        if i + 1 < len && bytes[i] == b'0' {
            let radix = bytes[i + 1];
            if radix == b'x' || radix == b'X' {
                i += 2;
                while i < len && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                    i += 1;
                }
                self.finish_number(i);
                return;
            }
            if radix == b'b' || radix == b'B' {
                i += 2;
                while i < len && (matches!(bytes[i], b'0' | b'1') || bytes[i] == b'_') {
                    i += 1;
                }
                self.finish_number(i);
                return;
            }
        }
        while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
        if i < len && bytes[i] == b'.' && !(i + 1 < len && bytes[i + 1] == b'.') {
            i += 1;
            while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
        }
        if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
            let mut j = i + 1;
            if j < len && (bytes[j] == b'+' || bytes[j] == b'-') {
                j += 1;
            }
            if j < len && bytes[j].is_ascii_digit() {
                i = j;
                while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                    i += 1;
                }
            }
        }
        self.finish_number(i);
    }

    fn finish_number(&mut self, mut i: usize) {
        let bytes = self.bytes;
        let len = bytes.len();
        while i < len {
            match bytes[i] {
                b'u' | b'U' | b'l' | b'L' | b'f' | b'F' | b'd' | b'D' | b'm' | b'M' => i += 1,
                _ => break,
            }
        }
        self.advance_to(i);
    }

    fn scan_char(&mut self) {
        let start = self.pos;
        self.advance_char();
        while let Some(ch) = self.peek_char() {
            match ch {
                '\\' => {
                    self.advance_char();
                    if self.peek_char().is_some() {
                        self.advance_char();
                    }
                }
                '\'' => {
                    self.advance_char();
                    return;
                }
                '\n' | '\r' => {
                    self.errors.push(LexError {
                        message: "unterminated char literal".into(),
                        offset: start,
                        line: self.line,
                    });
                    return;
                }
                _ => self.advance_char(),
            }
        }
        self.errors.push(LexError {
            message: "unterminated char literal".into(),
            offset: start,
            line: self.line,
        });
    }

    fn quote_run(&self) -> usize {
        let mut n = 0;
        while self.bytes.get(self.pos + n) == Some(&b'"') {
            n += 1;
        }
        n
    }

    /// Scan any string literal form. Handles nested literals in interpolation holes.
    fn scan_string_literal(&mut self) {
        let start = self.pos;
        let mut dollars = 0usize;
        let mut verbatim = false;
        // Prefixes.
        match self.peek_char() {
            Some('@') => {
                verbatim = true;
                self.advance_char();
                if self.peek_char() == Some('$') {
                    self.advance_char();
                    dollars = 1;
                }
            }
            Some('$') => {
                while self.peek_char() == Some('$') {
                    dollars += 1;
                    self.advance_char();
                }
                if self.peek_char() == Some('@') {
                    verbatim = true;
                    self.advance_char();
                }
            }
            _ => {}
        }
        let quotes = self.quote_run();
        if quotes >= 3 {
            self.scan_raw_string(start, dollars, quotes);
            return;
        }
        if self.peek_char() != Some('"') {
            self.errors.push(LexError {
                message: "malformed string literal".into(),
                offset: start,
                line: self.line,
            });
            self.advance_char();
            return;
        }
        self.advance_char(); // opening quote
        let interpolated = dollars > 0;
        loop {
            let Some(ch) = self.peek_char() else {
                self.errors.push(LexError {
                    message: "unterminated string literal".into(),
                    offset: start,
                    line: self.line,
                });
                break;
            };
            match ch {
                '"' => {
                    if verbatim && self.peek_byte(1) == Some(b'"') {
                        self.advance_to(self.pos + 2);
                    } else {
                        self.advance_char();
                        break;
                    }
                }
                '\\' if !verbatim => {
                    self.advance_char();
                    if self.peek_char().is_some() {
                        self.advance_char();
                    }
                }
                '\n' | '\r' if !verbatim => {
                    self.errors.push(LexError {
                        message: "newline in string literal".into(),
                        offset: start,
                        line: self.line,
                    });
                    break;
                }
                '{' if interpolated => {
                    if self.peek_byte(1) == Some(b'{') {
                        self.advance_to(self.pos + 2);
                    } else {
                        self.advance_char();
                        self.scan_interpolation_hole(start, 1, false);
                    }
                }
                '}' if interpolated => {
                    if self.peek_byte(1) == Some(b'}') {
                        self.advance_to(self.pos + 2);
                    } else {
                        self.advance_char();
                    }
                }
                _ => self.advance_char(),
            }
        }
        self.consume_utf8_suffix();
    }

    fn scan_raw_string(&mut self, start: usize, dollars: usize, quotes: usize) {
        // Opening delimiter.
        self.advance_to(self.pos + quotes);
        loop {
            let Some(ch) = self.peek_char() else {
                self.errors.push(LexError {
                    message: "unterminated raw string literal".into(),
                    offset: start,
                    line: self.line,
                });
                return;
            };
            match ch {
                '"' => {
                    let run = self.quote_run();
                    self.advance_to(self.pos + run);
                    if run >= quotes {
                        break;
                    }
                }
                '{' if dollars > 0 => {
                    let run = self.brace_run('{');
                    if run == dollars {
                        self.advance_to(self.pos + run);
                        self.scan_interpolation_hole(start, dollars, true);
                    } else {
                        self.advance_to(self.pos + run);
                    }
                }
                _ => self.advance_char(),
            }
        }
        self.consume_utf8_suffix();
    }

    fn brace_run(&self, brace: char) -> usize {
        let mut n = 0;
        for ch in self.src[self.pos..].chars() {
            if ch == brace {
                n += 1;
            } else {
                break;
            }
        }
        n
    }

    /// Scan an interpolation hole. `self.pos` is just past the opening brace(s).
    fn scan_interpolation_hole(&mut self, string_start: usize, dollars: usize, raw: bool) {
        let mut depth = 1usize;
        while let Some(ch) = self.peek_char() {
            match ch {
                '"' => self.scan_string_literal(),
                '\'' => self.scan_char(),
                '/' if self.peek_byte(1) == Some(b'/') => self.scan_line_comment(),
                '/' if self.peek_byte(1) == Some(b'*') => self.scan_block_comment(),
                '{' => {
                    let run = if raw { self.brace_run('{') } else { 1 };
                    depth += 1;
                    self.advance_to(self.pos + run);
                }
                '}' => {
                    let run = if raw { self.brace_run('}') } else { 1 };
                    self.advance_to(self.pos + run);
                    if raw && run == dollars {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return;
                        }
                    } else if !raw {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return;
                        }
                    }
                }
                _ => self.advance_char(),
            }
        }
        self.errors.push(LexError {
            message: "unterminated interpolation hole".into(),
            offset: string_start,
            line: self.line,
        });
    }

    fn consume_utf8_suffix(&mut self) {
        if self.starts_with("u8") {
            self.advance_to(self.pos + 2);
        }
    }

    fn scan_punctuation(&mut self) {
        for punct in PUNCTUATION {
            if self.starts_with(punct) {
                self.advance_to(self.pos + punct.len());
                return;
            }
        }
        self.advance_char();
    }
}

/// Returns true when `text` is a C# reserved or contextual keyword understood by the lexer.
#[must_use]
pub fn keyword(text: &str) -> bool {
    is_keyword(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<&str> {
        lex(src)
            .tokens
            .iter()
            .map(|t| t.text(src))
            .collect::<Vec<_>>()
    }

    #[test]
    fn lexes_basic_program() {
        let src = "using System;\nclass C { void M() { int x = 1 + 2; } }";
        let out = texts(src);
        assert_eq!(
            out,
            vec![
                "using", "System", ";", "class", "C", "{", "void", "M", "(", ")", "{", "int", "x",
                "=", "1", "+", "2", ";", "}", "}"
            ]
        );
        assert!(lex(src).errors.is_empty());
    }

    #[test]
    fn tracks_gaps_and_lines() {
        let src = "a\n\n    b c";
        let toks = lex(src).tokens;
        assert_eq!(toks[0].line, 1);
        assert_eq!(toks[1].line, 3);
        assert_eq!(toks[1].gap.newlines, 2);
        assert_eq!(toks[1].gap.spaces, 4);
        assert_eq!(toks[2].gap.newlines, 0);
        assert_eq!(toks[2].gap.spaces, 1);
    }

    #[test]
    fn lexes_all_string_forms() {
        let src = r#"var a = "x\"y"; var b = @"a""b"; var c = $"v={x}"; var d = $@"{x}""";"#;
        let out = texts(src);
        assert!(out.contains(&r#""x\"y""#));
        assert!(out.contains(&r#"@"a""b""#));
        assert!(out.contains(&r#"$"v={x}""#));
        assert!(lex(src).errors.is_empty(), "{:?}", lex(src).errors);
    }

    #[test]
    fn lexes_nested_interpolation_strings() {
        let src = r#"var s = $"{(x ? "a" : "b")}";"#;
        let out = texts(src);
        assert!(out.contains(&r#"$"{(x ? "a" : "b")}""#), "{out:?}");
        assert!(lex(src).errors.is_empty(), "{:?}", lex(src).errors);
    }

    #[test]
    fn lexes_raw_strings() {
        let src = "var s = \"\"\"\n  {not} \" interpolation\n  \"\"\";";
        let out = texts(src);
        assert_eq!(out[3], "\"\"\"\n  {not} \" interpolation\n  \"\"\"");
        assert!(lex(src).errors.is_empty());
    }

    #[test]
    fn lexes_raw_interpolated_strings() {
        let src = "var s = $$\"\"\"{{ x + 1 }}\"\"\";";
        let out = texts(src);
        assert_eq!(out[3], "$$\"\"\"{{ x + 1 }}\"\"\"");
        assert!(lex(src).errors.is_empty(), "{:?}", lex(src).errors);
    }

    #[test]
    fn lexes_directives_as_lines() {
        let src = "#if DEBUG\nvar x = 1;\n#endif\n";
        let toks = lex(src).tokens;
        assert_eq!(toks[0].kind, TokenKind::Directive);
        assert_eq!(toks[0].text(src), "#if DEBUG");
        assert_eq!(toks[6].kind, TokenKind::Directive);
    }

    #[test]
    fn lexes_numbers_without_eating_ranges() {
        let src = "1..2; 0xFFu; 1.5e-3f; .5m; 1_000;";
        let out = texts(src);
        assert_eq!(
            out,
            vec![
                "1", "..", "2", ";", "0xFFu", ";", "1.5e-3f", ";", ".5m", ";", "1_000", ";"
            ]
        );
    }

    #[test]
    fn comment_tokens_preserve_text() {
        let src = "// hi\n/* multi\n line */ x";
        let toks = lex(src).tokens;
        assert_eq!(toks[0].kind, TokenKind::LineComment);
        assert_eq!(toks[0].text(src), "// hi");
        assert_eq!(toks[1].kind, TokenKind::BlockComment);
        assert_eq!(toks[1].text(src), "/* multi\n line */");
        assert_eq!(toks[2].line, 3);
    }

    #[test]
    fn punctuation_longest_match() {
        let out = texts("a >>>= b; c ??= d; e?[0]?.X; f..g;");
        assert!(out.contains(&">>>="));
        assert!(out.contains(&"??="));
        assert!(out.contains(&"?["));
        assert!(out.contains(&"?."));
        assert!(out.contains(&".."));
    }

    #[test]
    fn lexes_utf8_literals() {
        let out = texts(r#"var b = "hi"u8;"#);
        assert!(out.contains(&r#""hi"u8"#));
    }

    #[test]
    fn never_panics_on_garbage() {
        for src in [
            "\"unterminated",
            "/* unterminated",
            "${\"x\"",
            "'''",
            "\u{feff}",
            "@",
            "\\",
            "0x",
            "$$\"\"\"{{",
        ] {
            let _ = lex(src);
        }
    }
}
