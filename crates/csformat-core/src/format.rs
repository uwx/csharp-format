//! Layout engine: turns the token stream into formatted source.
//!
//! The engine never rewrites token text. It only decides, for every token, whether it
//! starts a new line, how far it is indented, and whether a space precedes it. That makes
//! the transformation trivially lossless: the sequence of tokens (and therefore all
//! comments, directives and literals) is preserved exactly.

use crate::config::{BraceCategory, EndOfLine, FormatOptions, IndentUnit, WrapStyle};
use crate::lexer::{Token, TokenKind, lex};
use crate::syntax::{
    AngleKind, AttributePlacement, BraceKind, BraceOwner, BracketKind, ColonKind, CommaKind,
    OperatorKind, ParenKind, SemicolonKind, Syntax,
};

/// Placeholder for a line break inserted by the emitter.
const SEPARATOR: char = '\u{1}';

/// Result of formatting one document.
#[derive(Clone, Debug, Default)]
pub struct FormatResult {
    pub output: String,
    pub changed: bool,
    pub lex_errors: usize,
    pub parse_errors: usize,
}

/// Format a complete C# document.
///
/// When the input already contains parse errors, tree-sitter's error recovery can make a
/// single pass non-canonical (the reformatted text parses differently). In that case we
/// reformat until the output is a fixed point so the idempotency guarantee holds; if it
/// never stabilises we leave the source untouched.
#[must_use]
pub fn format_source(source: &str, options: &FormatOptions) -> FormatResult {
    let result = format_once(source, options);
    if result.output == source {
        return result;
    }

    // Some layout decisions depend on the shape of the previous pass (for example, whether a
    // declaration spans several lines). Iterate to a fixed point so formatting is idempotent
    // even when the first pass reshapes a construct.
    let lex_errors = result.lex_errors;
    let parse_errors = result.parse_errors;
    let mut output = result.output;
    let mut stable = false;
    for _ in 0..3 {
        let next = format_once(&output, options).output;
        if next == output {
            stable = true;
            break;
        }
        output = next;
    }

    if !stable {
        return FormatResult {
            changed: false,
            output: source.to_owned(),
            lex_errors,
            parse_errors,
        };
    }

    FormatResult {
        changed: output != source,
        output,
        lex_errors,
        parse_errors,
    }
}

/// One formatting pass.
fn format_once(source: &str, options: &FormatOptions) -> FormatResult {
    let mut lexed = lex(source);
    let syntax = Syntax::parse(source);
    syntax.annotate(source, &mut lexed.tokens);

    let lex_errors = lexed.errors.len();
    let parse_errors = syntax.error_count();

    if lexed.tokens.is_empty() {
        let mut output = String::new();
        if lexed.has_bom && options.prefer_bom != Some(false) {
            output.push('\u{feff}');
        }
        return FormatResult {
            changed: output != source,
            output,
            lex_errors,
            parse_errors,
        };
    }

    let layout = Layout::new(source, &lexed.tokens, options);
    let mut emitter = Emitter::new(source, &lexed.tokens, options, &layout);
    emitter.run();

    let output = finish_output(emitter.out, source, lexed.has_bom, options);
    FormatResult {
        changed: output != source,
        output,
        lex_errors,
        parse_errors,
    }
}

/// Precomputed facts about the token stream that the emitter needs.
struct Layout {
    /// For every brace token, the index of its partner (`usize::MAX` when unbalanced).
    brace_match: Vec<usize>,
    /// For every opening brace, whether the block was written on a single line and should
    /// stay that way.
    brace_inline: Vec<bool>,
    /// Whether the token sits inside (or closes) an inline block.
    inside_inline: Vec<bool>,
    /// `true` for a `<` that opens a generic argument or type parameter list.
    generic_open: Vec<bool>,
    /// `true` for a `>`/`>>`/`>>>` that closes such a list.
    generic_close: Vec<bool>,
    /// `true` for a token that may start a wrapped continuation line.
    wrap_candidate: Vec<bool>,
    /// For every `(`/`[`/`{`, the matching closer (and the reverse for closers).
    match_close: Vec<usize>,
    /// Innermost enclosing `(`, `[` or `{` for every token (`usize::MAX` at top level).
    enclosing_open: Vec<usize>,
    /// Whether a space precedes each token; mirrors `space_between`, index 0 is `false`.
    space_before: Vec<bool>,
}

impl Layout {
    fn new(source: &str, tokens: &[Token], options: &FormatOptions) -> Self {
        let n = tokens.len();
        let mut brace_match = vec![usize::MAX; n];
        let mut brace_inline = vec![false; n];
        let mut inside_inline = vec![false; n];

        let mut stack: Vec<usize> = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            match token.text(source) {
                "{" => stack.push(i),
                "}" => {
                    if let Some(open) = stack.pop() {
                        brace_match[open] = i;
                        brace_match[i] = open;
                    }
                }
                _ => {}
            }
        }

        if options.preserve_single_line_blocks {
            for open in 0..n {
                if tokens[open].text(source) != "{" {
                    continue;
                }
                let close = brace_match[open];
                if close == usize::MAX {
                    continue;
                }
                let mut single_line = true;
                for token in &tokens[open + 1..=close] {
                    if token.gap.newlines > 0
                        || token.text(source).contains('\n')
                        || token.text(source).contains('\r')
                        || token.kind == TokenKind::LineComment
                        || token.kind == TokenKind::Directive
                    {
                        single_line = false;
                        break;
                    }
                }
                let kind = tokens[open].ast.brace;
                let owner = tokens[open].ast.brace_owner;
                // ReSharper (Reformat Code, defaults) keeps object/collection
                // initializers, auto-property accessor lists, accessor bodies and
                // lambda bodies on one line, but always expands type, namespace,
                // method and control-flow blocks.
                let eligible = match kind {
                    BraceKind::Initializer | BraceKind::Pattern | BraceKind::SwitchExpression => {
                        true
                    }
                    // Unclassified braces come from ERROR nodes (constructs tree-sitter
                    // cannot parse). Keep them as written instead of inventing a block
                    // layout, which would only widen the parse-error region.
                    BraceKind::Other | BraceKind::None => true,
                    BraceKind::AccessorList => accessor_list_is_auto(source, tokens, open, close),
                    BraceKind::Block => matches!(
                        owner,
                        BraceOwner::Accessor | BraceOwner::Lambda | BraceOwner::AnonymousMethod
                    ),
                    _ => false,
                };
                brace_inline[open] = single_line && eligible;
            }
        }

        let mut inline_depth = 0usize;
        for (i, token) in tokens.iter().enumerate() {
            let text = token.text(source);
            let mut closing_inline = false;
            if text == "}" {
                let open = brace_match[i];
                if open != usize::MAX && brace_inline[open] {
                    closing_inline = true;
                    inline_depth = inline_depth.saturating_sub(1);
                }
            }
            inside_inline[i] = inline_depth > 0 || closing_inline;
            if text == "{" && brace_inline[i] {
                inline_depth += 1;
            }
        }

        let mut generic_open = vec![false; n];
        let mut generic_close = vec![false; n];
        let mut depth = 0i32;
        for i in 0..n {
            let text = tokens[i].text(source);
            if text == "<" {
                let ast = tokens[i].ast;
                let fallback = looks_like_generic_open(source, tokens, i)
                    && match ast.angle {
                        AngleKind::None | AngleKind::Operator => {
                            ast.operator != OperatorKind::Binary
                                || generic_close_ahead(source, tokens, i)
                        }
                        _ => false,
                    };
                let opens = matches!(
                    ast.angle,
                    AngleKind::GenericOpen | AngleKind::TypeParameterOpen
                ) || fallback;
                if opens {
                    generic_open[i] = true;
                    depth += 1;
                }
            } else if !text.is_empty() && text.bytes().all(|b| b == b'>') {
                let ast = tokens[i].ast;
                let closes = depth > 0
                    || matches!(
                        ast.angle,
                        AngleKind::GenericClose | AngleKind::TypeParameterClose
                    );
                if closes {
                    generic_close[i] = true;
                    depth = (depth - text.len() as i32).max(0);
                }
            }
        }

        let mut wrap_candidate = vec![false; n];
        for i in 1..n {
            let token = &tokens[i];
            if token.is_trivia() || token.kind == TokenKind::Directive || inside_inline[i] {
                continue;
            }
            let prev = &tokens[i - 1];
            if prev.is_trivia() {
                continue;
            }
            let pt = prev.text(source);
            let ct = token.text(source);
            if pt.contains('\n') || ct.contains('\n') {
                continue;
            }
            // Only break where a structural newline is not already implied and where
            // `is_continuation` will indent the following run of tokens.
            if matches!(
                pt,
                "{" | ";" | "}" | ":" | "else" | "catch" | "finally" | "(" | "["
            ) {
                continue;
            }
            if matches!(ct, "{" | "}" | ";" | ":" | ",") {
                continue;
            }
            let after_comma = pt == ","
                && matches!(
                    prev.ast.comma,
                    CommaKind::ArgumentList
                        | CommaKind::ParameterList
                        | CommaKind::AttributeArguments
                        | CommaKind::Tuple
                        | CommaKind::CollectionExpression
                        | CommaKind::Other
                );
            let before_operator = options.wrap.before_binary_opsign
                && token.ast.operator == OperatorKind::Binary
                && !is_assignment_operator(ct);
            let after_operator = !options.wrap.before_binary_opsign
                && prev.ast.operator == OperatorKind::Binary
                && !is_assignment_operator(pt);
            let after_assignment = !options.wrap.before_eq && is_assignment_operator(pt);
            let before_assignment = options.wrap.before_eq && is_assignment_operator(ct);
            let before_ternary =
                options.wrap.before_ternary_opsigns && token.ast.operator == OperatorKind::Ternary;
            let before_arrow = options.wrap.before_arrow_with_expressions && ct == "=>";
            let before_member =
                matches!(ct, "." | "?." | "->") && options.wrap.chained_method_calls;
            let after_dot =
                options.wrap.after_dot_in_method_calls && matches!(pt, "." | "?." | "->");
            let before_extends = options.wrap.before_extends_colon
                && ct == ":"
                && token.ast.colon == ColonKind::BaseList;
            let before_constraint =
                options.wrap.before_first_type_parameter_constraint && ct == "where";
            let before_langle = options.wrap.before_type_parameter_langle
                && ct == "<"
                && token.ast.angle == AngleKind::TypeParameterOpen;
            let before_linq = options.wrap.before_linq_expression
                && matches!(
                    ct,
                    "from" | "where" | "select" | "orderby" | "group" | "join" | "let"
                );
            wrap_candidate[i] = after_comma
                || before_operator
                || after_operator
                || after_assignment
                || before_assignment
                || before_ternary
                || before_arrow
                || before_member
                || after_dot
                || before_extends
                || before_constraint
                || before_langle
                || before_linq;
        }

        let mut match_close = vec![usize::MAX; n];
        let mut enclosing_open = vec![usize::MAX; n];
        let mut open_stack: Vec<usize> = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            let text = token.text(source);
            enclosing_open[i] = open_stack.last().copied().unwrap_or(usize::MAX);
            match text {
                "(" | "[" | "{" => open_stack.push(i),
                ")" | "]" | "}" => {
                    if let Some(open) = open_stack.pop() {
                        match_close[open] = i;
                        match_close[i] = open;
                    }
                }
                _ => {}
            }
        }

        let mut layout = Self {
            brace_match,
            brace_inline,
            inside_inline,
            generic_open,
            generic_close,
            wrap_candidate,
            match_close,
            enclosing_open,
            space_before: Vec::new(),
        };
        layout.space_before = (0..n)
            .map(|i| i > 0 && space_between(source, tokens, &layout, options, i))
            .collect();
        layout
    }

    fn inline_close(&self, i: usize) -> bool {
        let open = self.brace_match[i];
        open != usize::MAX && self.brace_inline[open]
    }
}

/// True when every accessor in an accessor list has a semicolon body (an auto-property).
///
/// `{ get; set; }` is kept on one line; `{ get => x; set { } }` is expanded.
fn accessor_list_is_auto(source: &str, tokens: &[Token], open: usize, close: usize) -> bool {
    tokens[open + 1..close]
        .iter()
        .all(|token| !matches!(token.text(source), "{" | "=>"))
}

/// Assignment-like operators that end a prefix when wrapped after.
fn is_assignment_operator(text: &str) -> bool {
    matches!(
        text,
        "=" | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "&="
            | "|="
            | "^="
            | "<<="
            | ">>="
            | ">>>="
            | "??="
            | "=>"
    )
}

/// Heuristic for `<` when tree-sitter did not classify the angle brackets (usually in code
/// that already contains parse errors).
fn looks_like_generic_open(source: &str, tokens: &[Token], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).map(|p| &tokens[p]) else {
        return false;
    };
    let prev_text = prev.text(source);
    if matches!(
        prev_text,
        "return" | "is" | "as" | "and" | "or" | "not" | "when" | "operator" | "case"
    ) {
        return false;
    }
    let Some(next) = tokens.get(i + 1) else {
        return false;
    };
    let next_text = next.text(source);
    if matches!(
        next_text,
        "=" | "<" | ">" | ";" | ")" | "]" | "}" | "," | "." | "|" | "&"
    ) {
        return false;
    }
    let named = matches!(
        prev.kind,
        TokenKind::Ident | TokenKind::Keyword | TokenKind::Number | TokenKind::String
    ) || matches!(prev_text, ">" | ">>" | ">>>" | "]" | ")" | "*");
    named
        && (next.kind == TokenKind::Ident
            || next.kind == TokenKind::Keyword
            || matches!(next_text, "(" | "[" | "@" | "?"))
}

/// Whether the `<` at `i` opens a type-argument list whose closing `>` is followed by a
/// shape that only a generic invocation/type can produce (`(`, `.`, `?.`, `[`, `)`, `?`).
fn generic_close_ahead(source: &str, tokens: &[Token], i: usize) -> bool {
    let mut angle = 0i32;
    let mut group = 0i32;
    for j in i..tokens.len() {
        let text = tokens[j].text(source);
        if !text.is_empty() && text.bytes().all(|b| b == b'>') {
            if group > 0 {
                continue;
            }
            angle -= text.len() as i32;
            if angle <= 0 {
                return tokens.get(j + 1).is_some_and(|next| {
                    matches!(
                        next.text(source),
                        "(" | "." | "?." | "[" | ")" | "?" | "!" | "," | ";" | "{"
                    ) || next.kind == TokenKind::Ident
                });
            }
            continue;
        }
        match text {
            "<" => angle += 1,
            "(" | "[" => group += 1,
            ")" | "]" => {
                if group == 0 {
                    return false;
                }
                group -= 1;
            }
            ";" | "&&" | "||" | "=" | "=>" | "{" | "}" if group == 0 => return false,
            _ => {}
        }
    }
    false
}

/// Formatting state while emitting one document.
struct Emitter<'a> {
    src: &'a str,
    tokens: &'a [Token],
    options: &'a FormatOptions,
    layout: &'a Layout,
    out: String,
    stack: Vec<Frame>,
    /// Line indents of the open `(`/`[` delimiters, newest last.
    paren_stack: Vec<ParenFrame>,
    /// Visual column of the current line.
    line_len: usize,
    /// Indent level of the line currently being written.
    cur_line_indent: i32,
    /// Visual column of the current line's indentation.
    cur_line_column: i32,
    /// Indent of the line that opened the most recently closed `(`/`]`.
    last_closed_paren_indent: Option<i32>,
    /// The current line started at a preserved (source-aligned) continuation column.
    preserved_line: bool,
    /// Indentation of the last emitted code line (directives excluded), in columns.
    last_code_indent: i32,
    /// Column of the last emitted code line's indentation (directives excluded).
    last_code_column: i32,
    /// Whether the last emitted code line continued a logical statement.
    continuation_active: bool,
    at_line_start: bool,
    seq: u32,
    /// For each opener, whether the group is chopped one element per line.
    group_chop: Vec<Option<bool>>,
    /// Elements of each group that have started on the current line.
    group_elems: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    open_indent: i32,
    content_indent: i32,
    kind: BraceKind,
    case_indent: Option<i32>,
    seq: u32,
}

#[derive(Clone, Copy, Debug)]
struct ParenFrame {
    /// Indentation column of the line on which the delimiter was written.
    indent: i32,
    kind: ParenKind,
    seq: u32,
}

/// Wrapping policy for a comma-separated list.
#[derive(Clone, Copy, Debug)]
struct GroupSpec {
    style: WrapStyle,
    max_on_line: Option<usize>,
    after_lpar: bool,
    before_rpar: bool,
    before_comma: bool,
    /// ReSharper keeps a simple single-line initializer/pattern inline even with
    /// `chop_always`; only chop it when it is multiline or does not fit.
    keeps_simple_inline: bool,
}

impl<'a> Emitter<'a> {
    fn new(
        src: &'a str,
        tokens: &'a [Token],
        options: &'a FormatOptions,
        layout: &'a Layout,
    ) -> Self {
        Self {
            src,
            tokens,
            options,
            layout,
            out: String::with_capacity(src.len() + src.len() / 8),
            stack: Vec::new(),
            paren_stack: Vec::new(),
            seq: 0,
            line_len: 0,
            cur_line_indent: 0,
            cur_line_column: 0,
            last_closed_paren_indent: None,
            preserved_line: false,
            last_code_indent: 0,
            last_code_column: 0,
            continuation_active: false,
            at_line_start: true,
            group_chop: vec![None; tokens.len()],
            group_elems: vec![0; tokens.len()],
        }
    }

    fn run(&mut self) {
        for i in 0..self.tokens.len() {
            self.emit_token(i);
        }
    }

    fn emit_token(&mut self, i: usize) {
        let token = &self.tokens[i];
        let text = token.text(self.src);
        let is_close = text == "}";
        let inline_close = is_close && self.layout.inline_close(i);
        let popped = if is_close && !inline_close {
            self.stack.pop()
        } else {
            None
        };

        let mut newlines = if i == 0 { 0 } else { self.newlines_before(i) };
        let wrapped = newlines == 0 && i > 0 && !self.at_line_start && self.should_wrap(i);
        if wrapped {
            newlines = 1;
        }
        let closing_paren_indent = if matches!(text, ")" | "]") {
            let indent = self.paren_stack.pop().map(|frame| frame.indent);
            self.last_closed_paren_indent = indent;
            indent
        } else {
            None
        };
        let indent = self.line_indent(i, newlines, popped, wrapped, closing_paren_indent);
        let preserved = if newlines > 0 && !wrapped {
            self.preserved_continuation_column(i)
        } else {
            None
        };

        if i > 0 {
            if newlines > 0 {
                for _ in 0..newlines {
                    self.out.push(SEPARATOR);
                }
                self.at_line_start = true;
            } else if !self.at_line_start {
                let space = self.should_space(i);
                if space {
                    self.out.push(' ');
                    self.line_len += 1;
                }
            }
        }

        if newlines > 0 || i == 0 {
            let code_column = if let Some(column) = preserved {
                self.write_indent_columns(column);
                self.preserved_line = true;
                column
            } else {
                self.write_indent(indent);
                self.preserved_line = false;
                indent
            };
            if token.kind == TokenKind::Directive {
                // Directives are emitted at column zero but must not disturb the alignment
                // used by the following code line.
                self.cur_line_column = self.last_code_column;
                self.cur_line_indent = self.last_code_indent / self.indent_unit_columns().max(1);
            } else {
                self.last_code_indent = code_column;
                self.last_code_column = code_column;
                self.continuation_active = wrapped
                    || self.is_continuation(i)
                    || (newlines > 0 && self.continuation_active);
            }
        }

        let raw = token.text(self.src);
        let reindented;
        let text = if token.kind == TokenKind::String && raw.contains('\n') {
            reindented = self.reindent_raw_string(raw);
            reindented.as_str()
        } else {
            raw
        };
        self.out.push_str(text);
        self.update_position(text);
        self.at_line_start = false;

        if matches!(text, "(" | "[" | "{") {
            self.decide_chop(i);
        }
        self.update_group_counts(i, newlines);

        if text == ":" && token.ast.colon == ColonKind::SwitchLabel {
            let labels = self.extra_columns(self.options.indent_switch_labels);
            let contents = self.extra_columns(self.options.indent_case_contents);
            if let Some(frame) = self.stack.last_mut() {
                frame.case_indent = Some(frame.open_indent + labels + contents);
            }
        }

        if text == "{" && !self.layout.brace_inline[i] {
            let open_indent = self.cur_line_column;
            let unit = self.indent_unit_columns();
            self.seq += 1;
            self.stack.push(Frame {
                open_indent,
                content_indent: open_indent + unit,
                kind: token.ast.brace,
                case_indent: None,
                seq: self.seq,
            });
        }

        if matches!(text, "(" | "[") {
            self.seq += 1;
            self.paren_stack.push(ParenFrame {
                indent: self.cur_line_column,
                kind: token.ast.paren,
                seq: self.seq,
            });
        }
    }

    /// Re-indent a multi-line raw string so its content aligns with the opening quotes.
    ///
    /// The closing delimiter defines how much indentation is stripped from the value, so
    /// shifting every line by the same amount preserves the string exactly.
    fn reindent_raw_string(&self, text: &str) -> String {
        let mut prefix = 0;
        let bytes = text.as_bytes();
        while prefix < bytes.len() && bytes[prefix] == b'$' {
            prefix += 1;
        }
        if !text[prefix..].starts_with("\"\"\"") {
            return text.to_string();
        }
        let Some(last_break) = text.rfind('\n') else {
            return text.to_string();
        };
        let (head, tail) = text.split_at(last_break + 1);
        let closing = tail.trim_start_matches([' ', '\t']);
        if !closing.starts_with("\"\"\"") || !closing.bytes().all(|b| b == b'"') {
            return text.to_string();
        }
        let old_indent = tail.chars().count() - closing.chars().count();
        let target = self.line_len + prefix;
        let pad = self.indent_columns(target);

        let mut out = String::with_capacity(text.len() + 16);
        let body = head.strip_suffix('\n').unwrap_or(head);
        for (line_index, line) in body.split('\n').enumerate() {
            if line_index > 0 {
                out.push('\n');
            }
            if line_index == 0 {
                out.push_str(line);
                continue;
            }
            // The `\r` of a CRLF terminator belongs to the end of this line; emit it after
            // the content so internal line endings stay exactly as authored.
            let (line, terminator) = match line.strip_suffix('\r') {
                Some(content) => (content, "\r"),
                None => (line, ""),
            };
            let mut removed = 0;
            let mut rest = line;
            while removed < old_indent {
                match rest.as_bytes().first() {
                    Some(b' ') => {
                        rest = &rest[1..];
                        removed += 1;
                    }
                    Some(b'\t') => {
                        rest = &rest[1..];
                        removed += self.options.tab_width.max(1);
                    }
                    _ => break,
                }
            }
            if !rest.is_empty() {
                out.push_str(&pad);
                out.push_str(rest);
            }
            out.push_str(terminator);
        }
        out.push('\n');
        out.push_str(&pad);
        out.push_str(closing);
        out
    }

    /// Spaces/tabs representing a visual column, honouring the configured indent unit.
    fn indent_columns(&self, columns: usize) -> String {
        match self.options.indent_unit() {
            IndentUnit::Spaces(_) => " ".repeat(columns),
            IndentUnit::Tabs => {
                let width = self.options.tab_width.max(1);
                let tabs = columns / width;
                let spaces = columns % width;
                "\t".repeat(tabs) + &" ".repeat(spaces)
            }
        }
    }

    fn write_indent(&mut self, columns: i32) {
        let columns = columns.max(0) as usize;
        let pad = self.indent_columns(columns);
        self.out.push_str(&pad);
        self.line_len = columns;
        let unit = self.indent_unit_columns().max(1) as usize;
        self.cur_line_indent = i32::try_from(columns / unit).unwrap_or(i32::MAX);
        self.cur_line_column = columns as i32;
    }

    fn update_position(&mut self, text: &str) {
        if let Some(pos) = text.rfind('\n') {
            let tail = &text[pos + 1..];
            self.line_len = tail.chars().count();
            self.at_line_start = tail.is_empty();
        } else if let Some(pos) = text.rfind('\r') {
            let tail = &text[pos + 1..];
            self.line_len = tail.chars().count();
            self.at_line_start = tail.is_empty();
        } else {
            self.line_len += text.chars().count();
        }
    }

    /// Maximum newlines allowed between two tokens at position `i`. ReSharper keeps at most
    /// `keep_in_declarations` blank lines between type members and `keep_in_code` inside
    /// method bodies.
    fn blank_cap_at(&self, _i: usize) -> u32 {
        let declarations = matches!(
            self.stack.last().map(|frame| frame.kind),
            None | Some(BraceKind::TypeBody | BraceKind::NamespaceBody | BraceKind::EnumBody)
        );
        let keep = if declarations {
            self.options.blank_lines.keep_in_declarations
        } else {
            self.options.blank_lines.keep_in_code
        };
        keep as u32 + 1
    }

    /// Width of one indentation level in columns.
    fn indent_unit_columns(&self) -> i32 {
        let unit = match self.options.indent_unit() {
            IndentUnit::Spaces(size) => size.max(1),
            IndentUnit::Tabs => self.options.tab_width.max(1),
        };
        i32::try_from(unit).unwrap_or(i32::MAX)
    }

    /// Extra columns for an on/off indentation flag.
    fn extra_columns(&self, enabled: bool) -> i32 {
        if enabled {
            self.indent_unit_columns()
        } else {
            0
        }
    }

    /// Indentation (in columns) for a token that starts a line.
    fn line_indent(
        &self,
        i: usize,
        newlines: u32,
        popped: Option<Frame>,
        wrapped: bool,
        closing_paren_indent: Option<i32>,
    ) -> i32 {
        let unit = self.indent_unit_columns();
        let token = &self.tokens[i];
        if token.kind == TokenKind::Directive {
            // `#region`/`#endregion` are indented with the surrounding code; conditional
            // compilation directives stay at column zero.
            let text = token.text(self.src);
            if text.starts_with("#region") || text.starts_with("#endregion") {
                return self.stack.last().map_or(0, |frame| frame.content_indent);
            }
            return 0;
        }
        if newlines > 0 {
            // A closing delimiter on its own line aligns with the line that opened it.
            if let Some(column) = closing_paren_indent {
                return column;
            }
        }
        if let Some(frame) = popped {
            return frame.open_indent;
        }
        let base = self
            .stack
            .last()
            .map(|frame| frame.case_indent.unwrap_or(frame.content_indent))
            .unwrap_or(0);

        if self.is_case_label(i) {
            return self
                .stack
                .last()
                .map(|frame| {
                    frame.open_indent + self.extra_columns(self.options.indent_switch_labels)
                })
                .unwrap_or(base);
        }
        // A comment that introduces a `case` label sits at the label's indent.
        if token.kind.is_comment()
            && self
                .next_code(i)
                .is_some_and(|next| self.is_case_label(next))
        {
            if let Some(frame) = self.stack.last() {
                if frame.kind == BraceKind::SwitchStatement {
                    return frame.open_indent
                        + self.extra_columns(self.options.indent_switch_labels);
                }
            }
        }

        let mut indent = base;
        let paren_inner = match (self.paren_stack.last(), self.stack.last()) {
            (Some(paren), Some(frame)) => paren.seq > frame.seq,
            (Some(_), None) => true,
            _ => false,
        };
        if paren_inner
            && newlines > 0
            && !self.is_statement_label(i)
            && !matches!(token.text(self.src), "{" | "}")
        {
            // Lines inside a broken argument/parameter list indent one level past the line
            // that opened the delimiter, not past the enclosing block.
            let open = self.paren_stack.last().map_or(base, |frame| frame.indent);
            return open + unit;
        }
        // A collection expression that starts its own line is block-like: the opening `[`
        // sits at the statement indent and its elements indent one level.
        if newlines > 0
            && token.text(self.src) == "["
            && token.ast.bracket == BracketKind::CollectionExpression
            && self.collection_multiline(i)
        {
            return base;
        }
        if newlines > 0 && token.text(self.src) == "{" {
            // A block written directly under a `case` label aligns with the label, not with
            // the case contents (ReSharper's switch formatting).
            if let Some(frame) = self.stack.last() {
                if frame.kind == BraceKind::SwitchStatement
                    && i > 0
                    && self.tokens[i - 1].text(self.src) == ":"
                    && self.tokens[i - 1].ast.colon == ColonKind::SwitchLabel
                {
                    return frame.open_indent
                        + self.extra_columns(self.options.indent_switch_labels);
                }
            }
            // Object/collection/`with` initializers and switch-expression bodies align with
            // the expression's line, which may itself be a continuation.
            if matches!(
                token.ast.brace,
                BraceKind::Initializer | BraceKind::SwitchExpression
            ) {
                return self.cur_line_column;
            }
            // Declaration bodies align with the declaration, even when it ends with a
            // wrapped constraint clause.
            if matches!(
                token.ast.brace,
                BraceKind::TypeBody
                    | BraceKind::NamespaceBody
                    | BraceKind::EnumBody
                    | BraceKind::AccessorList
            ) {
                return base;
            }
            // Control-flow and lambda bodies align with the line that introduced them. A
            // header ending in `)`/`]` may itself be a brace-less nested body, so use the
            // line that opened the delimiter.
            if token.ast.brace_owner == BraceOwner::Control
                || matches!(
                    token.ast.brace_owner,
                    BraceOwner::Lambda | BraceOwner::AnonymousMethod
                )
            {
                if matches!(
                    token.ast.brace_owner,
                    BraceOwner::Lambda | BraceOwner::AnonymousMethod
                ) {
                    // A lambda body inside a broken call argument list indents one level
                    // past the line that opened the call.
                    if let Some(paren) = self.paren_stack.last() {
                        if matches!(
                            paren.kind,
                            ParenKind::ArgumentList
                                | ParenKind::ParameterList
                                | ParenKind::AttributeArguments
                        ) {
                            return paren.indent + unit;
                        }
                    }
                    return self.cur_line_column;
                }
                let after_header = self
                    .prev_code(i)
                    .is_some_and(|p| matches!(self.tokens[p].text(self.src), ")" | "]"));
                if after_header {
                    if let Some(column) = self.last_closed_paren_indent {
                        return column;
                    }
                }
                return self.cur_line_column;
            }
            // Declaration bodies and bare blocks align with their enclosing scope.
            return base;
        }
        if newlines > 0
            && matches!(token.text(self.src), "else" | "catch" | "finally")
            && self
                .prev_code(i)
                .is_some_and(|p| self.tokens[p].text(self.src) == "}")
        {
            // These keywords align with the closing brace of the construct above them, which
            // may itself sit in a brace-less nested body.
            return self.cur_line_column;
        }
        if self.is_statement_label(i) {
            indent = match self.options.indent_labels {
                crate::config::LabelIndent::OneLessThanCurrent => base - unit,
                crate::config::LabelIndent::FlushLeft => 0,
                crate::config::LabelIndent::NoChange => base,
            };
        } else if newlines > 0 && (wrapped || self.is_continuation(i)) {
            indent += self.wrap_columns();
        }
        if newlines > 0 && self.is_continuation(i) && self.directive_between_previous_code(i) {
            // Preprocessor directives do not interrupt a declaration: `#else` between two
            // items of a base list must not reset the continuation indent to the body level.
            indent = self.last_code_indent;
        }
        indent
    }

    /// Whether a directive sits between token `i` and the previous non-trivia token.
    fn directive_between_previous_code(&self, i: usize) -> bool {
        let mut p = i;
        while p > 0 {
            p -= 1;
            let kind = self.tokens[p].kind;
            if kind == TokenKind::Directive {
                return true;
            }
            if !self.tokens[p].is_trivia() {
                return false;
            }
        }
        false
    }

    /// Effective line-length limit for wrapping, when enabled.
    fn wrap_limit(&self) -> Option<usize> {
        if !self.options.wrap.enabled {
            return None;
        }
        self.options
            .wrap
            .max_length
            .or(self.options.max_line_length)
    }

    /// Extra indentation (in levels) applied to continuation lines.
    fn wrap_columns(&self) -> i32 {
        let unit = self.indent_unit_columns();
        let columns = i32::try_from(self.options.continuation_indent_size).unwrap_or(i32::MAX);
        let total = columns.max(unit)
            * i32::try_from(self.options.wrap.continuation_indent)
                .unwrap_or(1)
                .max(1);
        total
    }

    /// First token of the contiguous run of attribute sections that ends at `close`.
    fn attribute_run_start(&self, close: usize) -> usize {
        let mut close = close;
        loop {
            let open = self.matching_open_bracket(close);
            if open > 0 && self.tokens[open - 1].text(self.src) == "]" {
                close = open - 1;
            } else {
                return open;
            }
        }
    }

    fn matching_open_bracket(&self, close: usize) -> usize {
        let mut depth = 0i32;
        let mut j = close;
        loop {
            match self.tokens[j].text(self.src) {
                "]" => depth += 1,
                "[" => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
            if j == 0 {
                return 0;
            }
            j -= 1;
        }
    }

    /// Whether the attribute section list starting at `start` is long enough that ReSharper
    /// moves the decorated declaration onto its own line.
    fn attribute_list_long(&self, start: usize) -> bool {
        let mut len = 0usize;
        let mut depth = 0i32;
        let mut j = start;
        while j < self.tokens.len() {
            let text = self.tokens[j].text(self.src);
            let is_open = text == "[";
            let is_close = text == "]";
            if is_open {
                depth += 1;
            }
            if j > start && self.tokens[j].gap.newlines == 0 && self.should_space(j) {
                len += 1;
            }
            len += text.chars().count();
            if is_close {
                depth -= 1;
                if depth == 0 {
                    match self.next_code(j) {
                        Some(next)
                            if self.tokens[next].text(self.src) == "["
                                && self.tokens[next].gap.newlines == 0 =>
                        {
                            j = next;
                            continue;
                        }
                        _ => break,
                    }
                }
            }
            j += 1;
        }
        // The space between the attribute list and the declaration counts too.
        len + 1 >= self.options.space.max_attribute_length_for_same_line
    }

    /// Whether the attribute section ending at `close` (a `]`) spans several lines.
    fn attribute_section_multiline(&self, close: usize) -> bool {
        let mut depth = 0i32;
        let mut j = close;
        loop {
            match self.tokens[j].text(self.src) {
                "]" => depth += 1,
                "[" => {
                    depth -= 1;
                    if depth == 0 {
                        return self.tokens[j].line != self.tokens[close].line;
                    }
                }
                _ => {}
            }
            if j == 0 {
                return false;
            }
            j -= 1;
        }
    }

    /// Whether the declaration starting at `i` occupies more than one source line. Used to
    /// decide whether property/indexer attributes move to their own line.
    fn declaration_multiline(&self, i: usize) -> bool {
        let start_line = self.tokens[i].line;
        let mut depth = 0i32;
        let mut j = i;
        while j < self.tokens.len() {
            match self.tokens[j].text(self.src) {
                "{" | "(" | "[" => depth += 1,
                "}" | ")" | "]" => {
                    depth = (depth - 1).max(0);
                    if depth == 0 && self.tokens[j].text(self.src) == "}" {
                        // A property body can be followed by `= initializer;`.
                        let next = self.next_code(j);
                        if next.is_some_and(|n| self.tokens[n].text(self.src) == "=") {
                            j = next.unwrap_or(j);
                        } else {
                            return self.tokens[j].line != start_line;
                        }
                    }
                }
                ";" if depth == 0 => return self.tokens[j].line != start_line,
                _ => {}
            }
            j += 1;
        }
        false
    }

    /// Whether the collection expression opened at `i` spans more than one line.
    fn collection_multiline(&self, i: usize) -> bool {
        let mut depth = 0i32;
        for j in i..self.tokens.len() {
            if j > i && self.tokens[j].gap.newlines > 0 {
                return true;
            }
            match self.tokens[j].text(self.src) {
                "[" => depth += 1,
                "]" => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Existing continuation lines keep the author's column. ReSharper honours these
    /// alignments when it keeps a line break (binary/boolean operands align under the
    /// expression start), and recomputing them needs a full alignment engine.
    fn preserved_continuation_column(&self, i: usize) -> Option<i32> {
        let token = &self.tokens[i];
        if token.gap.newlines == 0 || token.kind == TokenKind::Directive {
            return None;
        }
        if token.kind.is_comment() {
            // A comment inside a continued expression stays where the author put it.
            if !self
                .next_code(i)
                .is_some_and(|next| self.is_continuation(next))
            {
                return None;
            }
        } else if matches!(
            token.ast.brace,
            BraceKind::Pattern | BraceKind::Initializer | BraceKind::SwitchExpression
        ) {
            // Braces that belong to an expression (patterns, initializers, switch bodies)
            // keep the author's column; their contents and closing brace follow from it.
        } else if !self.is_continuation(i) {
            return None;
        }
        let width = self.options.tab_width.max(1);
        let columns = token.gap.tabs as usize * width + token.gap.spaces as usize;
        Some(i32::try_from(columns).unwrap_or(i32::MAX))
    }

    fn write_indent_columns(&mut self, columns: i32) {
        self.write_indent(columns);
    }

    /// Whether token `i` should start a wrapped line because the *next* run of tokens
    /// would not fit on the current one.
    fn should_wrap(&self, i: usize) -> bool {
        // Chopped lists and per-line element limits force breaks regardless of the
        // current line length.
        if self.group_break(i) {
            return true;
        }
        // Lines that already were continuations of an aligned expression in the source are
        // left alone; ReSharper keeps their layout.
        if self.preserved_line {
            return false;
        }
        // ReSharper does not break right after a lambda arrow.
        if i > 0 && self.tokens[i - 1].text(self.src) == "=>" {
            return false;
        }
        let Some(limit) = self.wrap_limit() else {
            return false;
        };
        if !self.layout.wrap_candidate[i] || self.layout.inside_inline[i] {
            return false;
        }
        // Switch-expression arms keep their source layout (the whole expression is usually
        // already formatted by ReSharper).
        if self
            .stack
            .last()
            .is_some_and(|frame| frame.kind == BraceKind::SwitchExpression)
        {
            return false;
        }
        let token = &self.tokens[i];
        if token.is_trivia()
            || token.kind == TokenKind::Directive
            || token.ast.switch_section_inline
        {
            return false;
        }

        let mut width = usize::from(self.should_space(i));
        let mut j = i;
        while j < self.tokens.len() {
            let current = &self.tokens[j];
            if j > i {
                if self.layout.wrap_candidate[j] || self.newlines_before(j) > 0 {
                    break;
                }
                if self.should_space(j) {
                    width += 1;
                }
            }
            let text = current.text(self.src);
            // Tokens that already span several lines cannot be moved onto one line;
            // wrapping before such a run would not shorten it.
            if text.contains('\n') || text.contains('\r') {
                return false;
            }
            width += text.chars().count();
            if width > limit {
                break;
            }
            j += 1;
        }
        self.line_len + width > limit
    }

    fn is_continuation(&self, i: usize) -> bool {
        if i == 0 {
            return false;
        }
        // Comments and directives do not interrupt a continued expression; look through them
        // so a base list or argument list keeps its continuation indentation.
        let mut p = i - 1;
        while p > 0 && self.tokens[p].is_trivia() {
            p -= 1;
        }
        let prev = &self.tokens[p];
        let cur = &self.tokens[i];
        let pt = prev.text(self.src);
        let ct = cur.text(self.src);

        if prev.is_trivia() || cur.is_trivia() {
            return false;
        }
        if pt == "{" {
            return false;
        }
        if pt == ";" {
            if prev.ast.semicolon == SemicolonKind::ForStatement {
                // A broken `for` header continues on the next line.
                return true;
            }
            return false;
        }
        if pt == "}" {
            // Braces that close a pattern, initializer or switch expression are part of the
            // surrounding expression, so the next line still continues it.
            if !matches!(
                prev.ast.brace,
                BraceKind::Pattern | BraceKind::Initializer | BraceKind::SwitchExpression
            ) {
                return false;
            }
        }
        if pt == ":" {
            return matches!(
                prev.ast.colon,
                ColonKind::Ternary
                    | ColonKind::BaseList
                    | ColonKind::Constraint
                    | ColonKind::ConstructorInitializer
                    | ColonKind::NamedArgument
            );
        }
        if pt == "," {
            // Commas inside argument/parameter lists do continue the expression; commas in
            // switch expressions / enum bodies start a new sibling item instead.
            return matches!(
                prev.ast.comma,
                CommaKind::ArgumentList
                    | CommaKind::ParameterList
                    | CommaKind::AttributeArguments
                    | CommaKind::Tuple
                    | CommaKind::CollectionExpression
                    | CommaKind::Initializer
                    | CommaKind::TypeArguments
                    | CommaKind::TypeParameters
                    | CommaKind::Other
            );
        }
        if matches!(ct, "}" | "{" | "else" | "catch" | "finally") {
            return false;
        }
        if ct == "while" && pt == "}" {
            return false;
        }
        if prev.ast.bracket == BracketKind::Attribute {
            return false;
        }
        if self.is_case_label(i) {
            return false;
        }
        true
    }

    fn is_case_label(&self, i: usize) -> bool {
        let token = &self.tokens[i];
        let text = token.text(self.src);
        if text == "case" {
            if i > 0 && self.tokens[i - 1].text(self.src) == "goto" {
                return false;
            }
            return true;
        }
        if text == "default" {
            let Some(next) = self.next_code(i) else {
                return false;
            };
            if self.tokens[next].text(self.src) != ":" {
                return false;
            }
            if self.tokens[next].ast.colon == ColonKind::SwitchLabel {
                return true;
            }
            return self
                .stack
                .last()
                .is_some_and(|frame| frame.kind == BraceKind::SwitchStatement);
        }
        false
    }

    fn is_statement_label(&self, i: usize) -> bool {
        let token = &self.tokens[i];
        if token.kind != TokenKind::Ident {
            return false;
        }
        let Some(next) = self.next_code(i) else {
            return false;
        };
        self.tokens[next].text(self.src) == ":" && self.tokens[next].ast.colon == ColonKind::Label
    }

    fn next_code(&self, i: usize) -> Option<usize> {
        (i + 1..self.tokens.len()).find(|&j| !self.tokens[j].is_trivia())
    }

    /// Previous non-trivia token index.
    fn prev_code(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&j| !self.tokens[j].is_trivia())
    }

    /// Wrapping policy for the list delimited by `open`, if it is a list we wrap.
    fn group_spec(&self, open: usize) -> Option<GroupSpec> {
        let wrap = &self.options.wrap;
        let token = &self.tokens[open];
        let spec = |style: WrapStyle, max_on_line: Option<usize>, after_lpar, before_rpar| {
            Some(GroupSpec {
                style,
                max_on_line,
                after_lpar,
                before_rpar,
                before_comma: wrap.before_comma,
                keeps_simple_inline: false,
            })
        };
        match token.text(self.src) {
            "(" => match token.ast.paren {
                ParenKind::ArgumentList => spec(
                    wrap.arguments,
                    wrap.max_arguments_on_line,
                    wrap.after_invocation_lpar,
                    wrap.before_invocation_rpar,
                ),
                ParenKind::ParameterList | ParenKind::BracketedParameterList => spec(
                    wrap.parameters,
                    wrap.max_formal_parameters_on_line,
                    wrap.after_declaration_lpar,
                    wrap.before_declaration_rpar,
                ),
                _ => None,
            },
            "{" => match token.ast.brace {
                BraceKind::Initializer => {
                    let array = self
                        .prev_code(open)
                        .is_some_and(|p| self.tokens[p].text(self.src) == "]");
                    let mut spec = if array {
                        spec(
                            wrap.array_initializer,
                            wrap.max_array_initializer_elements_on_line,
                            false,
                            false,
                        )
                    } else {
                        spec(
                            wrap.object_initializer,
                            wrap.max_initializer_elements_on_line,
                            false,
                            false,
                        )
                    };
                    if let Some(spec) = spec.as_mut() {
                        spec.keeps_simple_inline = true;
                    }
                    spec
                }
                BraceKind::EnumBody => spec(
                    wrap.enum_declaration,
                    wrap.max_enum_members_on_line,
                    false,
                    false,
                ),
                BraceKind::SwitchExpression => {
                    let mut spec = spec(wrap.switch_expression, None, false, false);
                    if let Some(spec) = spec.as_mut() {
                        spec.keeps_simple_inline = true;
                    }
                    spec
                }
                BraceKind::Pattern => {
                    let mut spec = spec(wrap.property_pattern, None, false, false);
                    if let Some(spec) = spec.as_mut() {
                        spec.keeps_simple_inline = true;
                    }
                    spec
                }
                _ => None,
            },
            "[" => match token.ast.bracket {
                BracketKind::CollectionExpression | BracketKind::ListPattern => {
                    let mut spec = spec(wrap.list_pattern, None, false, false);
                    if let Some(spec) = spec.as_mut() {
                        spec.keeps_simple_inline = true;
                    }
                    spec
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Decide, when the opener has been written, whether this group is chopped.
    fn decide_chop(&mut self, open: usize) {
        let Some(spec) = self.group_spec(open) else {
            return;
        };
        let close = self.layout.match_close[open];
        if close == usize::MAX {
            return;
        }
        let multiline = (open + 1..=close).any(|j| self.tokens[j].gap.newlines > 0);
        let width: usize = (open..=close)
            .map(|j| {
                let len = self.tokens[j].text(self.src).chars().count();
                len + usize::from(j > open && self.layout.space_before[j])
            })
            .sum();
        let too_long = self
            .wrap_limit()
            .is_some_and(|limit| self.line_len + width > limit);
        let chop = match spec.style {
            WrapStyle::WrapIfLong => false,
            WrapStyle::ChopAlways if spec.keeps_simple_inline => multiline || too_long,
            WrapStyle::ChopAlways => true,
            WrapStyle::ChopIfLong => multiline || too_long,
        };
        self.group_chop[open] = Some(chop);
    }

    /// Forced break points inside a chopped group or a max-elements-per-line group.
    fn group_break(&self, i: usize) -> bool {
        let open = self.layout.enclosing_open[i];
        if open == usize::MAX || self.layout.inside_inline[i] {
            return false;
        }
        let Some(spec) = self.group_spec(open) else {
            return false;
        };
        let text = self.tokens[i].text(self.src);
        if self.group_chop[open] == Some(true) {
            if i == self.layout.match_close[open] && spec.before_rpar {
                return true;
            }
            if self.prev_code(i) == Some(open) && spec.after_lpar {
                return true;
            }
            if text == "," && spec.before_comma {
                return true;
            }
            if let Some(prev) = self.prev_code(i) {
                if !spec.before_comma
                    && self.tokens[prev].text(self.src) == ","
                    && self.layout.enclosing_open[prev] == open
                {
                    return true;
                }
            }
        }
        if let (Some(prev), Some(max)) = (self.prev_code(i), spec.max_on_line) {
            if self.tokens[prev].text(self.src) == ","
                && self.layout.enclosing_open[prev] == open
                && self.group_elems[open] >= max
            {
                return true;
            }
        }
        false
    }

    /// Reset per-line element counters for every group a new line starts inside, and
    /// count the element that starts at token `i`.
    fn update_group_counts(&mut self, i: usize, newlines: u32) {
        if newlines > 0 {
            let mut open = self.layout.enclosing_open[i];
            while open != usize::MAX {
                self.group_elems[open] = 0;
                open = self.layout.enclosing_open[open];
            }
        }
        let open = self.layout.enclosing_open[i];
        if open == usize::MAX {
            return;
        }
        let starts_element = self.prev_code(i).is_some_and(|prev| {
            prev == open
                || (self.tokens[prev].text(self.src) == ","
                    && self.layout.enclosing_open[prev] == open)
        });
        if starts_element {
            self.group_elems[open] += 1;
        }
    }

    fn newlines_before(&self, i: usize) -> u32 {
        let token = &self.tokens[i];
        let prev = &self.tokens[i - 1];
        let pt = prev.text(self.src);
        let ct = token.text(self.src);
        let gap = token.gap.newlines;
        let in_inline = self.layout.inside_inline[i];
        let cap = self.blank_cap_at(i);

        // Region directives get a blank line inside their body by default.
        if ct.starts_with("#region") && pt != "{" {
            if let Some(blank) = self.options.blank_lines.before_region {
                return (blank as u32 + 1).min(cap);
            }
        }
        if ct.starts_with("#endregion") {
            if let Some(blank) = self.options.blank_lines.before_endregion {
                return (blank as u32 + 1).min(cap);
            }
        }
        if prev.kind == TokenKind::Directive && (pt == "#region" || pt.starts_with("#region ")) {
            if let Some(blank) = self.options.blank_lines.after_region {
                return (blank as u32 + 1).min(cap);
            }
        }

        // Directives always own a line, but keep a blank line the author left before them.
        if token.kind == TokenKind::Directive {
            return gap.max(1).min(cap);
        }
        if prev.kind == TokenKind::Directive {
            return gap.max(1).min(cap);
        }
        if prev.kind == TokenKind::LineComment {
            return gap.max(1).min(cap);
        }
        if token.kind == TokenKind::LineComment && gap > 0 {
            return gap.min(cap);
        }

        // Trailing line comments stay on the line they were written on.
        if token.kind == TokenKind::LineComment && gap == 0 && !(pt == "{" && !in_inline) {
            return 0;
        }

        // Keep a block comment on the line it was written on.
        if token.kind == TokenKind::BlockComment && gap == 0 && !(pt == "{" && !in_inline) {
            return 0;
        }

        // A block comment that starts a line keeps the following code on a new line.
        if prev.kind == TokenKind::BlockComment && gap == 0 && prev.gap.newlines > 0 {
            return 1;
        }

        // Attribute sections follow ReSharper's per-member placement: fields, properties,
        // events and indexers keep them on the declaration line, while types, methods,
        // local functions and the rest put them on their own line. An XML doc comment
        // above the member always forces the attribute onto its own line.
        if pt == "]" && prev.ast.member_attribute && ct == "[" {
            // Consecutive attribute sections on one line stay together.
        } else if pt == "]" && prev.ast.member_attribute {
            let start = self.attribute_run_start(i - 1);
            let too_long = self.attribute_list_long(start);
            let placement = match prev.ast.attribute {
                // Fields and event fields stay on the declaration line unless the attribute
                // list itself is too long to fit.
                AttributePlacement::SameLine => {
                    if too_long {
                        AttributePlacement::OwnLine
                    } else {
                        AttributePlacement::SameLine
                    }
                }
                AttributePlacement::SameLineIfSingleLine => {
                    if too_long
                        || self.member_has_doc_comment(i)
                        || self.attribute_section_multiline(i - 1)
                        || self.declaration_multiline(i)
                    {
                        AttributePlacement::OwnLine
                    } else {
                        AttributePlacement::SameLine
                    }
                }
                other => other,
            };
            match placement {
                AttributePlacement::SameLine => return 0,
                AttributePlacement::OwnLine => return 1,
                AttributePlacement::SameLineIfSingleLine | AttributePlacement::None => {}
            }
        }
        // Switch sections written on one line stay on one line (stacked labels still break).
        if prev.ast.switch_section
            && prev.ast.switch_section_inline
            && !self.is_case_label(i)
            && !matches!(ct, "}")
        {
            return 0;
        }
        // `break;` keeps the author's line: appended after the previous statement when it was
        // written that way, on its own line otherwise.
        if pt == ";" && ct == "break" && prev.ast.switch_section {
            return gap.min(cap);
        }

        if pt == "}" && matches!(ct, "else" | "catch" | "finally") {
            let force = match ct {
                "else" => self.options.newline.before_else,
                "catch" => self.options.newline.before_catch,
                _ => self.options.newline.before_finally,
            };
            // A blank line the author left between the brace and the keyword is preserved.
            return gap.max(u32::from(force)).min(cap);
        }
        if pt == "}" && ct == "while" {
            if self.while_ends_do_while(i) {
                return 0;
            }
            // A `while` statement after a block keeps the author's separation.
            return gap.max(1).min(cap);
        }

        // Member separation: ReSharper's defaults insert one blank line around any
        // member that spans multiple lines, and around type declarations even when
        // they are one-liners. One-line members of the same kind stay grouped.
        if self.is_member_start(i)
            && matches!(pt, "}" | ";")
            && !matches!(ct, "}" | ";" | ")" | ",")
        {
            let prev_multi = (pt == "}" && !self.layout.inline_close(i - 1))
                || prev.ast.type_declaration
                || (pt == ";" && self.ends_local_function(i - 1));
            let cur_multi = self.member_multiline(i)
                || token.ast.type_declaration
                || (token.ast.member_attribute
                    && (token.ast.attribute == AttributePlacement::OwnLine
                        || self.attribute_list_long(i)));
            let blank = self
                .options
                .blank_lines
                .between_members
                .map_or(u32::from(prev_multi || cur_multi || gap >= 2), |b| b as u32);
            if blank > 0 {
                return gap.max(blank + 1).min(cap);
            }
            return gap.max(1).min(cap);
        }

        // A multi-line statement terminated by `;` (do-while) or a local function
        // declaration is followed by a blank line.
        if pt == ";"
            && (prev.ast.do_statement || self.ends_local_function(i - 1))
            && !matches!(ct, "}")
        {
            return gap.max(2).min(cap);
        }

        if ct == "{" {
            if self.layout.brace_inline[i] {
                return gap;
            }
            if prev.kind == TokenKind::LineComment {
                return 1;
            }
            let category = brace_category(token);
            let force = u32::from(self.options.newline.before_open_brace.contains(category));
            if force > 0 {
                // A blank line the author left before the brace is preserved.
                return gap.max(force).min(cap);
            }
            return 0;
        }

        if ct == "}" {
            if in_inline {
                return gap;
            }
            if pt == "{" {
                return 1;
            }
            return 1;
        }

        if pt == "{" {
            if in_inline {
                return gap;
            }
            if token.kind == TokenKind::BlockComment && gap == 0 {
                return 0;
            }
            return 1;
        }

        if pt == ";" {
            if in_inline {
                return 0;
            }
            if prev.ast.semicolon == SemicolonKind::ForStatement {
                // A `for` header broken over several lines keeps its layout.
                return gap.min(1);
            }
            return gap.max(1).min(cap);
        }

        if self.is_case_label(i) {
            return if in_inline { 0 } else { gap.max(1).min(cap) };
        }

        if pt == ":" && matches!(prev.ast.colon, ColonKind::SwitchLabel | ColonKind::Label) {
            if in_inline {
                return 0;
            }
            // `case` labels keep the author's inline statements; goto labels own a line,
            // except when they are followed by an empty statement.
            if prev.ast.colon == ColonKind::Label && ct != ";" {
                return gap.max(1).min(cap);
            }
            return gap.min(cap);
        }

        if pt == "," && !in_inline {
            if prev.ast.comma == CommaKind::EnumBody {
                // Enum members always get their own line, but blank lines are preserved.
                return gap.max(1).min(cap);
            }
            if prev.ast.comma == CommaKind::SwitchExpression {
                // Switch-expression arms keep the author's line breaks.
                return gap.min(1);
            }
        }

        if pt == "}" {
            // Accessor bodies are kept inline, but each accessor starts on its own line.
            if prev.ast.brace == BraceKind::Block && prev.ast.brace_owner == BraceOwner::Accessor {
                return gap.max(1).min(cap);
            }
            // A broken argument/parameter list keeps its closing delimiter on its own line.
            if matches!(ct, ")" | "]") && gap > 0 {
                return gap.min(cap);
            }
            if self.layout.inline_close(i - 1) && gap == 0 {
                return 0;
            }
            if ct == ";" && prev.ast.brace == BraceKind::Block {
                // An empty statement after a block keeps its own line (`}` then `;`).
                return gap.min(cap);
            }
            if matches!(ct, ";" | "," | ")" | "]" | "." | "?." | "!" | "=") {
                return 0;
            }
            if token.kind.is_comment() {
                // A comment attaches to the code around it; keep the author's gap.
                return gap.max(1).min(cap);
            }
            if matches!(ct, "}" | ";") {
                return gap.max(1).min(cap);
            }
            // Braces that do not close a statement (object/collection initializers,
            // anonymous types, switch expressions, property patterns) keep the source gap.
            if matches!(
                prev.ast.brace_owner,
                BraceOwner::Initializer
                    | BraceOwner::AnonymousType
                    | BraceOwner::SwitchExpression
                    | BraceOwner::Pattern
                    | BraceOwner::None
            ) {
                return gap.min(cap);
            }
            // A multi-line statement is followed by a blank line (ReSharper default).
            return gap.max(2).min(cap);
        }

        if let Some(blank) = self.options.blank_lines.between_members {
            if self.is_member_start(i) && matches!(pt, "}" | ";") && !matches!(ct, "}" | ";") {
                return (blank as u32 + 1).min(cap);
            }
        }

        if pt == "#region" || pt.starts_with("#region ") {
            if let Some(blank) = self.options.blank_lines.after_region {
                return (blank as u32 + 1).min(cap);
            }
        }

        gap.min(cap)
    }

    fn while_ends_do_while(&self, i: usize) -> bool {
        let mut depth = 0i32;
        for j in i..self.tokens.len() {
            match self.tokens[j].text(self.src) {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return self
                            .next_code(j)
                            .is_some_and(|next| self.tokens[next].text(self.src) == ";");
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn is_member_start(&self, i: usize) -> bool {
        self.tokens[i].ast.member_start
    }

    /// Whether the declaration whose attribute section ends just before `i` is preceded by
    /// an XML doc comment (`///`), which forces its attributes onto their own line.
    fn member_has_doc_comment(&self, i: usize) -> bool {
        let mut k = i;
        while k > 0 && !self.tokens[k].ast.member_start {
            k -= 1;
        }
        if k == 0 {
            return false;
        }
        let prev = &self.tokens[k - 1];
        prev.kind == TokenKind::LineComment && prev.text(self.src).trim_start().starts_with("///")
    }

    /// Whether token `i` is the last token of a local function declaration. The
    /// `local_function` flag is set on every token inside the declaration, so this requires
    /// the token to close the body or terminate an expression-bodied declaration.
    fn ends_local_function(&self, i: usize) -> bool {
        let token = &self.tokens[i];
        if !token.ast.local_function {
            return false;
        }
        match token.text(self.src) {
            "}" => token.ast.brace_owner == BraceOwner::LocalFunction,
            ";" => {
                // An expression-bodied local function only gets a separating blank line
                // when its declaration spans several lines (for example, an attribute on
                // its own line).
                let multiline = self.local_function_start(i).is_some_and(|start| {
                    self.declaration_multiline(start)
                        || (self.tokens[start].ast.member_attribute
                            && self.tokens[start].ast.attribute == AttributePlacement::OwnLine)
                });
                multiline
                    && self
                        .tokens
                        .get(i + 1)
                        .is_none_or(|next| !next.ast.local_function)
            }
            _ => false,
        }
    }

    /// Index of the first token of the local function declaration that contains token `i`.
    fn local_function_start(&self, i: usize) -> Option<usize> {
        let mut j = i;
        loop {
            if self.tokens[j].ast.member_start {
                return Some(j);
            }
            if j == 0 {
                return None;
            }
            j -= 1;
        }
    }

    /// Whether the member (or local function) starting at `i` will span multiple lines.
    fn member_multiline(&self, i: usize) -> bool {
        let n = self.tokens.len();
        let mut j = i;
        while j < n {
            match self.tokens[j].text(self.src) {
                "{" => {
                    let close = self.layout.brace_match[j];
                    if close == usize::MAX || !self.layout.brace_inline[j] {
                        return true;
                    }
                    match self.tokens.get(close + 1).map(|t| t.text(self.src)) {
                        Some(";" | "}" | ",") => return false,
                        Some("=") => j = close + 1,
                        _ => return false,
                    }
                }
                "}" | ";" => return false,
                _ => j += 1,
            }
        }
        false
    }

    /// Whether a space separates token `i` from its predecessor on the same line.
    fn should_space(&self, i: usize) -> bool {
        space_between(self.src, self.tokens, self.layout, self.options, i)
    }
}

#[allow(clippy::too_many_arguments)]
fn space_between(
    src: &str,
    tokens: &[Token],
    layout: &Layout,
    options: &FormatOptions,
    i: usize,
) -> bool {
    let prev = &tokens[i - 1];
    let cur = &tokens[i];
    let pt = prev.text(src);
    let ct = cur.text(src);

    if prev.is_trivia() || cur.is_trivia() {
        if prev.kind == TokenKind::BlockComment && cur.gap.newlines == 0 {
            let before = if i >= 2 { tokens[i - 2].text(src) } else { "" };
            // A comment at statement position is separated from the statement; elsewhere
            // the author's spacing is kept (`for ( /**/; …)`).
            if matches!(before, "{" | ";" | "}" | "=>" | ":") {
                return true;
            }
            return cur.gap.spaces > 0 || cur.gap.tabs > 0;
        }
        return true;
    }

    // `(x)+y`, `(x)*y` are ambiguous with casts to pointer/type; ReSharper keeps the
    // source spacing instead of guessing, so do the same.
    let ambiguous_after_cast = |idx: usize| {
        tokens.get(idx).is_some_and(|t| {
            matches!(t.text(src), "*" | "+" | "-" | "&" | "^")
                && t.ast.operator == OperatorKind::UnaryPrefix
                && idx > 0
                && tokens[idx - 1].text(src) == ")"
                && tokens[idx - 1].ast.paren == ParenKind::Cast
        })
    };
    if ambiguous_after_cast(i) || ambiguous_after_cast(i - 1) {
        return cur.gap.spaces > 0 || cur.gap.tabs > 0;
    }

    // A numeric literal immediately followed by `.Member` lexes as `100.` + `Member`.
    if cur.kind == TokenKind::Ident && prev.kind == TokenKind::Number && pt.ends_with('.') {
        return false;
    }

    // `is { … } ? a : b` parses the `?` as plain punctuation; keep the source space.
    if ct == "?"
        && matches!(
            cur.ast.operator,
            OperatorKind::None | OperatorKind::Nullable
        )
        && pt == "}"
        && prev.ast.brace == BraceKind::Pattern
    {
        return cur.gap.spaces > 0 || cur.gap.tabs > 0;
    }

    // ReSharper preserves the author's spacing around the range operator `..`.
    if pt == ".." || ct == ".." {
        return cur.gap.spaces > 0 || cur.gap.tabs > 0;
    }

    if ct == "{" {
        return true;
    }
    if pt == "{" {
        return true;
    }
    if ct == "}" {
        return true;
    }
    if pt == "}" {
        if matches!(ct, "?" | ":") {
            // Ternary/question and case-label colons are decided by the operator rules below.
        } else {
            return !matches!(ct, ";" | "," | ")" | "]" | "." | "?." | "!");
        }
    }

    if layout.generic_open[i] || layout.generic_close[i] {
        return false;
    }
    if layout.generic_open[i - 1] {
        return false;
    }
    if layout.generic_close[i - 1] {
        return !matches!(
            ct,
            "(" | ")" | "]" | "[" | "," | ";" | "." | "?." | "?" | "!" | "->"
        );
    }

    if matches!(pt, "." | "->" | "::" | "?." | "?[")
        || matches!(ct, "." | "->" | "::" | "?." | "?[")
    {
        return false;
    }

    if ct == "," {
        return options.space.before_comma;
    }
    if pt == "," {
        // Multidimensional array ranks keep tight commas when empty (`new int[,]`) but
        // separate dimension sizes (`new int[7, 5]`).
        if prev.ast.comma == CommaKind::ArrayRank {
            return !matches!(ct, "," | "]");
        }
        return options.space.after_comma;
    }

    if pt == ";" {
        if ct == ";" || ct == ")" {
            return false;
        }
        if prev.ast.semicolon == SemicolonKind::ForStatement {
            return options.space.after_semicolon_in_for;
        }
        return true;
    }
    if ct == ";" {
        if pt == "(" || pt == ";" {
            return false;
        }
        if pt == ":" {
            return colon_after(prev, options);
        }
        return cur.ast.semicolon == SemicolonKind::ForStatement
            && options.space.before_semicolon_in_for;
    }

    if ct == ":" {
        return colon_before(cur, options);
    }
    if pt == ":" {
        return colon_after(prev, options);
    }

    if pt == "(" {
        if ct == ")" {
            return empty_paren_space(prev, options);
        }
        if prev.ast.paren == ParenKind::Cast {
            return false;
        }
        return options.space.between_parentheses;
    }
    // `(T)x` — no space between the cast and its operand unless configured.
    if pt == ")" && prev.ast.paren == ParenKind::Cast {
        return options.space.after_cast;
    }
    if ct == ")" {
        if pt == "(" {
            return empty_paren_space(prev, options);
        }
        return options.space.between_parentheses;
    }

    if pt == "[" {
        if ct == "]" {
            return options.space.between_empty_square_brackets;
        }
        return options.space.between_square_brackets;
    }
    if ct == "]" {
        if pt == "[" {
            return options.space.between_empty_square_brackets;
        }
        return options.space.between_square_brackets;
    }
    if ct == "[" {
        if pt == "new" {
            return false;
        }
        if pt == "]" && prev.ast.bracket == BracketKind::Attribute {
            return true;
        }
        return match cur.ast.bracket {
            BracketKind::ElementAccess | BracketKind::ArrayRank => false,
            BracketKind::Attribute => is_word(prev),
            BracketKind::Other => {
                // Tree-sitter sometimes parses `T x = [ … ]` as an element binding;
                // after an expression starter the `[` opens a collection.
                if matches!(
                    pt,
                    "=" | "(" | "," | "return" | "=>" | ":" | "[" | "{" | "?" | "??"
                ) {
                    true
                } else {
                    is_word(prev)
                }
            }
            BracketKind::CollectionExpression | BracketKind::ListPattern => {
                pt != "new" && !matches!(pt, "(" | "[" | "{" | "." | "?." | "!" | "~" | "->" | "::")
            }
            BracketKind::None => is_word(prev),
        };
    }

    if ct == "(" {
        return paren_space(src, tokens, layout, options, i);
    }

    if ct == "?" {
        return match cur.ast.operator {
            OperatorKind::Ternary => options.space.before_question_in_conditional,
            OperatorKind::Nullable | OperatorKind::None => false,
            _ => is_word(prev),
        };
    }
    if pt == "?" {
        // `x as Key? ?? fallback` keeps the coalescing operator separated.
        if cur.text(src) == "??" {
            return true;
        }
        return match prev.ast.operator {
            OperatorKind::Ternary => options.space.after_question_in_conditional,
            OperatorKind::Nullable | OperatorKind::None => is_word(cur),
            _ => is_word(cur),
        };
    }

    if cur.ast.operator == OperatorKind::Pointer {
        return false;
    }
    if prev.ast.operator == OperatorKind::Pointer {
        return is_word(cur);
    }

    if cur.ast.operator == OperatorKind::Range || prev.ast.operator == OperatorKind::Range {
        return false;
    }

    if is_prefix_unary(src, tokens, i) {
        if is_binary_op(src, tokens, i - 1) {
            return true;
        }
        return is_word(prev) || matches!(pt, ")" | "]" | "!");
    }
    if is_prefix_unary(src, tokens, i - 1) {
        return false;
    }
    if is_postfix_unary(src, tokens, i) {
        return false;
    }
    if is_postfix_unary(src, tokens, i - 1) {
        // `i++ == x`: the postfix operator still needs a space before a binary operator.
        return is_binary_op(src, tokens, i);
    }

    if pt == "operator" {
        return true;
    }

    if is_binary_op(src, tokens, i) || is_binary_op(src, tokens, i - 1) {
        return match options.space.around_binary_operators {
            crate::config::AroundBinary::BeforeAndAfter => true,
            crate::config::AroundBinary::None => false,
            crate::config::AroundBinary::Ignore => cur.gap.spaces > 0,
        };
    }

    if is_word(prev) || is_word(cur) {
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn paren_space(
    src: &str,
    tokens: &[Token],
    _layout: &Layout,
    options: &FormatOptions,
    i: usize,
) -> bool {
    let prev = &tokens[i - 1];
    let cur = &tokens[i];
    let pt = prev.text(src);
    let binary_before = is_binary_op(src, tokens, i - 1);

    if pt == "delegate" {
        // Anonymous method: `delegate(int x)`, never `delegate (int x)`.
        return false;
    }
    if pt == "new" {
        // `new (T a, T b)[…]` (tuple type) needs the space; `new()`, `new Foo(…)` and
        // `new string(…)` do not.
        return !matches!(cur.ast.paren, ParenKind::ArgumentList);
    }
    // A statement/expression directly after a control-flow header keeps a space.
    if prev.text(src) == ")"
        && matches!(
            prev.ast.paren,
            ParenKind::If
                | ParenKind::While
                | ParenKind::For
                | ParenKind::ForEach
                | ParenKind::Using
                | ParenKind::Lock
                | ParenKind::Fixed
                | ParenKind::Catch
                | ParenKind::Switch
        )
    {
        return true;
    }
    if i >= 2 && tokens[i - 2].text(src) == "new" {
        // `new string('a', 3)` — the type name belongs to the creation expression.
        return false;
    }
    if pt == "var" {
        // `var (a, b) = …` deconstruction declaration.
        return true;
    }
    if pt == "extension" {
        // C# 14 extension block: `extension(T receiver)`.
        return false;
    }

    // Text-based rules for constructs whose AST classification flips between passes
    // (typically inside already-broken code). They keep formatting idempotent.
    if prev.kind == TokenKind::Keyword {
        if matches!(
            pt,
            "if" | "while" | "for" | "foreach" | "switch" | "catch" | "using" | "lock" | "fixed"
        ) {
            return options.space.after_keywords_in_control_flow;
        }
        if matches!(pt, "checked" | "unchecked") {
            // `checked(expr)` is an expression; the keyword is glued to its operand.
            return false;
        }
        if matches!(
            pt,
            "typeof" | "sizeof" | "default" | "nameof" | "this" | "base" | "new"
        ) {
            return false;
        }
        if matches!(
            cur.ast.paren,
            ParenKind::ParameterList | ParenKind::BracketedParameterList
        ) {
            return options.space.method_declaration_name_and_open_paren;
        }
        if matches!(
            cur.ast.paren,
            ParenKind::ArgumentList | ParenKind::AttributeArguments
        ) {
            // Contextual keywords used as identifiers (`value()`, `field()`).
            return options.space.method_call_name_and_open_paren;
        }
        return true;
    }
    if matches!(cur.ast.paren, ParenKind::None | ParenKind::Other)
        && matches!(prev.kind, TokenKind::Ident)
    {
        return options.space.method_call_name_and_open_paren;
    }

    match cur.ast.paren {
        ParenKind::ArgumentList | ParenKind::AttributeArguments => {
            if pt == "new" {
                return false;
            }
            options.space.method_call_name_and_open_paren
        }
        ParenKind::ParameterList | ParenKind::BracketedParameterList => {
            options.space.method_declaration_name_and_open_paren
        }
        ParenKind::ConstructorInitializer => false,
        ParenKind::If
        | ParenKind::While
        | ParenKind::For
        | ParenKind::ForEach
        | ParenKind::Switch
        | ParenKind::Catch
        | ParenKind::Using
        | ParenKind::Lock
        | ParenKind::Fixed
        | ParenKind::Checked => options.space.after_keywords_in_control_flow,
        ParenKind::TypeOf | ParenKind::SizeOf | ParenKind::Default => false,
        ParenKind::Cast | ParenKind::Parenthesized | ParenKind::Tuple => {
            // Deliberately ignore `is_word(prev)`: a `(` right after an identifier is a
            // call/declaration that tree-sitter sometimes mislabels as a cast, and the
            // difference would make a second formatting pass change the output.
            binary_before || matches!(pt, "?" | ":" | "??" | "=>")
        }
        ParenKind::None | ParenKind::Other => {
            binary_before || is_word(prev) || matches!(pt, "?" | ":" | "??" | "=>")
        }
    }
}

fn is_word(token: &Token) -> bool {
    matches!(
        token.kind,
        TokenKind::Ident
            | TokenKind::Keyword
            | TokenKind::Number
            | TokenKind::String
            | TokenKind::Char
    )
}

fn empty_paren_space(open: &Token, options: &FormatOptions) -> bool {
    match open.ast.paren {
        ParenKind::ArgumentList => options.space.method_call_empty_parameter_list_parentheses,
        ParenKind::ParameterList => {
            options
                .space
                .method_declaration_empty_parameter_list_parentheses
        }
        _ => options.space.between_parentheses,
    }
}

fn colon_before(token: &Token, options: &FormatOptions) -> bool {
    match token.ast.colon {
        ColonKind::BaseList => options.space.before_colon_in_inheritance,
        ColonKind::Ternary => options.space.before_colon_in_conditional,
        ColonKind::Constraint | ColonKind::ConstructorInitializer => true,
        ColonKind::NamedArgument
        | ColonKind::SwitchLabel
        | ColonKind::Label
        | ColonKind::Pattern
        | ColonKind::AttributeTarget => false,
        ColonKind::None | ColonKind::Other => false,
    }
}

fn colon_after(token: &Token, options: &FormatOptions) -> bool {
    match token.ast.colon {
        ColonKind::BaseList => options.space.after_colon_in_inheritance,
        ColonKind::Ternary => options.space.after_colon_in_conditional,
        ColonKind::Constraint
        | ColonKind::NamedArgument
        | ColonKind::Pattern
        | ColonKind::ConstructorInitializer
        | ColonKind::AttributeTarget => true,
        ColonKind::SwitchLabel | ColonKind::Label => true,
        ColonKind::None | ColonKind::Other => true,
    }
}

fn brace_category(token: &Token) -> BraceCategory {
    if token.ast.brace == BraceKind::AccessorList {
        return BraceCategory::Accessors;
    }
    if token.ast.brace == BraceKind::SwitchExpression {
        return BraceCategory::Control;
    }
    match token.ast.brace_owner {
        BraceOwner::Method
        | BraceOwner::Constructor
        | BraceOwner::Destructor
        | BraceOwner::Operator => BraceCategory::Methods,
        BraceOwner::LocalFunction => BraceCategory::LocalFunctions,
        BraceOwner::Accessor => BraceCategory::Accessors,
        BraceOwner::Lambda => BraceCategory::Lambdas,
        BraceOwner::AnonymousMethod => BraceCategory::AnonymousMethods,
        BraceOwner::Type | BraceOwner::Enum | BraceOwner::Namespace => BraceCategory::Types,
        BraceOwner::Property => BraceCategory::Properties,
        BraceOwner::AnonymousType => BraceCategory::AnonymousTypes,
        BraceOwner::Initializer => BraceCategory::Initializers,
        BraceOwner::Control
        | BraceOwner::SwitchExpression
        | BraceOwner::Pattern
        | BraceOwner::Other
        | BraceOwner::None => BraceCategory::Control,
    }
}

fn is_binary_op(source: &str, tokens: &[Token], i: usize) -> bool {
    let Some(token) = tokens.get(i) else {
        return false;
    };
    if token.ast.operator == OperatorKind::Binary {
        return true;
    }
    if matches!(token.text(source), "<" | ">")
        && token.ast.angle == crate::syntax::AngleKind::Operator
    {
        // A relational `<`/`>` that tree-sitter reports as an angle operator.
        return true;
    }
    if token.ast.operator != OperatorKind::None {
        return false;
    }
    let text = token.text(source);
    if matches!(
        text,
        "=" | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "&="
            | "|="
            | "^="
            | "<<="
            | ">>="
            | ">>>="
            | "??="
            | "=="
            | "!="
            | "<="
            | ">="
            | "&&"
            | "||"
            | "??"
            | "=>"
    ) {
        return true;
    }
    if matches!(text, "<" | ">" | "<=" | ">=") {
        // Relational pattern operators after `and`/`or`/`is`/`not`.
        if i > 0
            && matches!(
                tokens[i - 1].text(source),
                "and" | "or" | "not" | "is" | "when"
            )
        {
            return true;
        }
    }
    if matches!(
        text,
        "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "<<" | ">>" | ">>>" | ">"
    ) {
        return i > 0 && is_value_like(source, tokens, i - 1);
    }
    if text == "<" {
        return i > 0
            && is_value_like(source, tokens, i - 1)
            && tokens.get(i + 1).is_some_and(|_| true);
    }
    false
}

fn is_value_like(source: &str, tokens: &[Token], i: usize) -> bool {
    let token = &tokens[i];
    if matches!(
        token.kind,
        TokenKind::Ident | TokenKind::Number | TokenKind::String | TokenKind::Char
    ) {
        return true;
    }
    if token.kind == TokenKind::Keyword {
        return matches!(
            token.text(source),
            "this" | "base" | "true" | "false" | "null" | "default" | "value"
        );
    }
    matches!(token.text(source), ")" | "]" | "}" | ">")
}

fn is_prefix_unary(source: &str, tokens: &[Token], i: usize) -> bool {
    let Some(token) = tokens.get(i) else {
        return false;
    };
    if token.ast.operator == OperatorKind::UnaryPrefix {
        return true;
    }
    if token.ast.operator != OperatorKind::None {
        return false;
    }
    let text = token.text(source);
    if !matches!(text, "+" | "-" | "!" | "~" | "++" | "--" | "*" | "&" | "^") {
        return false;
    }
    if matches!(text, "++" | "--") {
        return !(i > 0 && is_value_like(source, tokens, i - 1));
    }
    if text == "!" {
        // `!` is prefix unless it directly follows a value and is followed by an operator,
        // close delimiter or separator (the null-forgiving operator).
        if i > 0 && is_value_like(source, tokens, i - 1) {
            let next = tokens.get(i + 1).map(|t| t.text(source));
            if matches!(
                next,
                Some(";" | "," | ")" | "]" | "." | "?." | "=" | "=>" | "}" | ":")
            ) {
                return false;
            }
        }
        return true;
    }
    if text == "~" {
        // `~` is only ever the prefix (bitwise complement or destructor marker).
        return true;
    }
    i == 0 || !is_value_like(source, tokens, i - 1)
}

fn is_postfix_unary(source: &str, tokens: &[Token], i: usize) -> bool {
    let Some(token) = tokens.get(i) else {
        return false;
    };
    if token.ast.operator == OperatorKind::UnaryPostfix {
        return true;
    }
    if token.ast.operator != OperatorKind::None {
        return false;
    }
    let text = token.text(source);
    if matches!(text, "++" | "--") {
        return i > 0 && is_value_like(source, tokens, i - 1);
    }
    if text == "!" {
        return i > 0
            && is_value_like(source, tokens, i - 1)
            && !is_prefix_unary(source, tokens, i);
    }
    false
}

fn finish_output(mut out: String, source: &str, has_bom: bool, options: &FormatOptions) -> String {
    // Line breaks inserted by the emitter use a sentinel so that newlines inside token
    // text (raw strings, block comments) are never rewritten.
    let eol = match options.end_of_line {
        EndOfLine::Auto => detect_eol(source).unwrap_or("\n"),
        EndOfLine::Lf => "\n",
        EndOfLine::Crlf => "\r\n",
        EndOfLine::Cr => "\r",
    };
    if out.contains(SEPARATOR) {
        out = out.replace(SEPARATOR, eol);
    }

    if options.insert_final_newline {
        if !out.is_empty() && !out.ends_with(eol) {
            out.push_str(eol);
        }
    } else {
        while out.ends_with('\n') || out.ends_with('\r') {
            out.pop();
        }
    }

    let want_bom = options.prefer_bom.unwrap_or(has_bom);
    if want_bom && !out.starts_with('\u{feff}') {
        out.insert(0, '\u{feff}');
    } else if !want_bom && out.starts_with('\u{feff}') {
        out.remove(0);
    }
    out
}

fn detect_eol(source: &str) -> Option<&'static str> {
    let bytes = source.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        match byte {
            b'\n' => return Some("\n"),
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    return Some("\r\n");
                }
                return Some("\r");
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        format_source(source, &FormatOptions::default()).output
    }

    #[test]
    fn formats_simple_class() {
        let source = "class A{public void M(){int x=1;if(x>0){Console.WriteLine(x);}else{x=-1;}}}";
        let output = fmt(source);
        assert_eq!(
            output,
            "class A\n{\n    public void M()\n    {\n        int x = 1;\n        if (x > 0)\n        {\n            Console.WriteLine(x);\n        }\n        else\n        {\n            x = -1;\n        }\n    }\n}"
        );
    }

    #[test]
    fn preserves_single_line_accessor_lists() {
        // ReSharper's Reformat Code profile keeps accessor lists inline but expands
        // statement blocks (verified against jb cleanupcode 2026.2.4.1).
        let source = "class A { public int X { get; set; } public void M() { return; } }";
        let output = fmt(source);
        assert!(output.contains("public int X { get; set; }"), "{output}");
        assert!(
            output.contains("public void M()\n    {\n        return;\n    }"),
            "{output}"
        );
    }

    #[test]
    fn spaces_generics_and_nullable() {
        let source = "class A{List<Dictionary<int,string>>? F;void M(int?x){var y=x??0;var z=x is{Length:1};}}";
        let output = fmt(source);
        assert!(
            output.contains("List<Dictionary<int, string>>? F;"),
            "{output}"
        );
        assert!(output.contains("void M(int? x)"), "{output}");
        assert!(output.contains("x ?? 0"), "{output}");
        assert!(output.contains("x is { Length: 1 }"), "{output}");
    }

    #[test]
    fn formats_switch_statement_and_expression() {
        let source = "class A{int M(int x){switch(x){case 1:return 2;default:break;}return x switch{1=>10,_=>20};}}";
        let output = fmt(source);
        assert!(
            output.contains(
                "switch (x)\n        {\n            case 1: return 2;\n            default: break;\n        }"
            ),
            "{output}"
        );
        assert!(output.contains("x switch { 1 => 10, _ => 20 }"), "{output}");
    }

    #[test]
    fn formats_initializers_and_collection_expressions() {
        let source =
            "class A{void M(){var a=new[]{1,2,3};var b=new List<int>{1,2};var c=new C{A=1,B=2};}}";
        let output = fmt(source);
        assert!(output.contains("new[] { 1, 2, 3 }"), "{output}");
        assert!(output.contains("new List<int> { 1, 2 }"), "{output}");
        assert!(output.contains("new C { A = 1, B = 2 }"), "{output}");
    }

    #[test]
    fn handles_comments_and_directives() {
        let source = "#nullable enable\nclass A{// hi\nvoid M(){/* x */int y=1;}}";
        let output = fmt(source);
        assert!(
            output.starts_with("#nullable enable\nclass A\n{\n"),
            "{output}"
        );
        assert!(output.contains("// hi\n"), "{output}");
        assert!(output.contains("/* x */ int y = 1;"), "{output}");
    }

    #[test]
    fn is_idempotent() {
        let sources = [
            "class A{public void M(){int x=1;if(x>0){x++;}else{x--;}}}",
            "using System;\nnamespace N;\nrecord R(int X);\n",
            "class A{void M(){var s=$\"x={1+2}\";var r=\"\"\"raw\"\"\";}}",
            "class A{void M(){for(int i=0;i<10;i++){}}while(true){}do{}while(false);}}",
        ];
        for source in sources {
            let once = fmt(source);
            let twice = fmt(&once);
            assert_eq!(once, twice, "not idempotent for {source:?}");
        }
    }

    #[test]
    fn wraps_long_argument_lists() {
        let source = "class A { void M() { SomeMethodCall(argumentNumberOne, argumentNumberTwo, argumentNumberThree, argumentNumberFour, argumentNumberFive, argumentNumberSix, argumentNumberSeven, argumentNumberEight); } }";
        let output = fmt(source);
        assert!(
            output.lines().all(|line| line.chars().count() <= 120),
            "{output}"
        );
        assert!(
            output.contains(", \n") || output.contains(",\n"),
            "expected a wrapped argument list: {output}"
        );
        let twice = fmt(&output);
        assert_eq!(output, twice, "wrapping must be idempotent");
        let report = crate::verify::verify_source(source, &FormatOptions::default());
        assert!(report.lossless, "wrapping must be lossless");
    }

    #[test]
    fn wraps_long_assignments_and_operators() {
        let source = "class A { void M() { var result = firstOperand + secondOperand + thirdOperand + fourthOperand + fifthOperand + sixthOperand + seventhOperand + eighthOperand; } }";
        let output = fmt(source);
        assert!(
            output.lines().all(|line| line.chars().count() <= 120),
            "{output}"
        );
        assert!(output.contains("\n"), "{output}");
        assert_eq!(output, fmt(&output), "wrapping must be idempotent");
    }

    #[test]
    fn wrapping_can_be_disabled() {
        let source = "class A { void M() { SomeMethodCall(argumentNumberOne, argumentNumberTwo, argumentNumberThree, argumentNumberFour, argumentNumberFive, argumentNumberSix); } }";
        let mut options = FormatOptions::default();
        options.wrap.enabled = false;
        let output = format_source(source, &options).output;
        assert!(
            output.contains("SomeMethodCall(argumentNumberOne, argumentNumberTwo"),
            "{output}"
        );
    }

    #[test]
    fn preserves_all_tokens() {
        let cases = [
            "class A{void M(){var x=\"a\\\"b\";}}",
            "class A{void M(){var x=@\"a\"\"b\";}}",
            "class A{void M(){var x=\"\"\"\n  raw \"quoted\"\n  \"\"\";}}",
            "class A{void M(){var x=$@\"{1}\";}}",
            "class A{void M(){var x=$ \"\"\";{a} \"\"\";}}",
            "#if DEBUG\nclass A{#endif\n}",
        ];
        for source in cases {
            let report = crate::verify::verify_source(source, &FormatOptions::default());
            assert!(report.lossless, "token mismatch for {source:?}");
        }
    }

    #[test]
    fn preserves_parse_errors() {
        let source = "class A { void M( { int x = ; }";
        let output = fmt(source);
        assert!(lex(&output).errors.is_empty());
    }
}
