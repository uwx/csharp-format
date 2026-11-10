//! Layout engine: turns the token stream into formatted source.
//!
//! The engine never rewrites token text. It only decides, for every token, whether it
//! starts a new line, how far it is indented, and whether a space precedes it. That makes
//! the transformation trivially lossless: the sequence of tokens (and therefore all
//! comments, directives and literals) is preserved exactly.

use crate::config::{
    AlignIndentStyle, BraceCategory, EndOfLine, FormatOptions, IndentUnit, SpecialIndent, WrapStyle,
};
use crate::lexer::{Token, TokenKind, lex};
use crate::syntax::{
    AngleKind, AttributePlacement, BraceKind, BraceOwner, BracketKind, ColonKind, CommaKind,
    DeclKind, OperatorKind, ParenKind, SemicolonKind, StmtKind, Syntax, flags, should_separate,
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
    /// Whether the token sits inside a generic argument or type parameter list.
    inside_generics: Vec<bool>,
    /// For every token, the index of the `:` introducing its base list (`usize::MAX` when
    /// the token is not part of one).
    base_owner: Vec<usize>,
    /// For every token, the index of the first token of the declaration it belongs to
    /// (`usize::MAX` outside declarations).
    decl_owner: Vec<usize>,
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
                "{" | ";" | "}" | "else" | "catch" | "finally" | "(" | "["
            ) {
                continue;
            }
            // A base-list colon is a wrap point on either side of the colon.
            if pt == ":" && !matches!(prev.ast.colon, ColonKind::BaseList | ColonKind::Constraint) {
                continue;
            }
            if matches!(ct, "{" | "}" | ";" | ",") {
                continue;
            }
            if ct == ":" && token.ast.colon != ColonKind::BaseList {
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
                        | CommaKind::BaseList
                        | CommaKind::VariableDeclaration
                        | CommaKind::Constraints
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
            // Whether the break happens before or after the colon depends on the current
            // line, so `should_wrap` decides it; both sides are candidates.
            let before_extends = ct == ":" && token.ast.colon == ColonKind::BaseList;
            let after_extends = pt == ":" && prev.ast.colon == ColonKind::BaseList;
            // A constraint clause wraps as a unit, so `where` is always a break point.
            let before_constraint = ct == "where" && token.ast.has(flags::CONSTRAINT);
            let after_constraint_colon = pt == ":" && prev.ast.colon == ColonKind::Constraint;
            // `new()` in a constraint clause breaks before its `(` when `new` still fits.
            let before_ctor_paren =
                ct == "(" && token.ast.paren == ParenKind::ConstructorConstraint;
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
                || after_extends
                || before_constraint
                || after_constraint_colon
                || before_ctor_paren
                || before_langle
                || before_linq;
        }

        let mut match_close = vec![usize::MAX; n];
        let mut enclosing_open = vec![usize::MAX; n];
        let mut inside_generics = vec![false; n];
        let mut open_stack: Vec<usize> = Vec::new();
        let mut angle_depth = 0usize;
        for (i, token) in tokens.iter().enumerate() {
            let text = token.text(source);
            enclosing_open[i] = open_stack.last().copied().unwrap_or(usize::MAX);
            if generic_open[i] {
                angle_depth += 1;
                inside_generics[i] = true;
            } else if generic_close[i] {
                inside_generics[i] = angle_depth > 0;
                let closed = text.chars().filter(|c| *c == '>').count().max(1);
                angle_depth = angle_depth.saturating_sub(closed);
            } else {
                inside_generics[i] = angle_depth > 0;
            }
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

        // Track the `:` of a base list and the start of a variable declaration so the
        // emitter can chop `class C : A, B` and `int a = 1, b = 2` consistently.
        let mut base_owner = vec![usize::MAX; n];
        let mut decl_owner = vec![usize::MAX; n];
        let mut base_cur = usize::MAX;
        let mut decl_cur = usize::MAX;
        for i in 0..n {
            let text = tokens[i].text(source);
            match text {
                "{" | "}" | ";" | "where" => {
                    base_cur = usize::MAX;
                    decl_cur = usize::MAX;
                }
                "(" => decl_cur = usize::MAX,
                ")" | "]" => decl_cur = usize::MAX,
                _ => {}
            }
            if text == ":" && tokens[i].ast.colon == ColonKind::BaseList {
                base_cur = i;
            }
            if decl_cur == usize::MAX && !text.is_empty() && !matches!(text, "{" | "}" | ";") {
                decl_cur = i;
            }
            base_owner[i] = base_cur;
            decl_owner[i] = decl_cur;
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
            inside_generics,
            base_owner,
            decl_owner,
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
    /// Indent column of every statement that started a line, keyed by its start byte, so a
    /// nested (brace-less) statement can indent relative to its parent statement.
    stmt_indent: std::collections::HashMap<usize, i32>,
    /// Column where the expression of a statement starts, for alignment of continuation
    /// lines; keyed by the statement's start byte.
    expr_align: std::collections::HashMap<usize, i32>,
    /// Statement whose expression starts at the next emitted token (`=`, `return`, `=>`).
    pending_align: Option<usize>,
    /// Column of the first entry of a base list, keyed by its colon's token index.
    base_align: std::collections::HashMap<usize, i32>,
    /// Base-list colon whose first entry starts at the next emitted token.
    pending_base: Option<usize>,
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
    /// For each opener, whether a per-line element limit wrapped the group (which also
    /// honours the `after_lpar`/`before_rpar` preferences).
    group_open_break: Vec<bool>,
    /// Whether the layout broke a line somewhere inside a delimiter.
    wrapped_inside: Vec<bool>,
    /// For each base-list colon, whether the list is chopped one entry per line.
    base_chop: Vec<Option<bool>>,
    /// For each base-list colon, whether the colon itself starts a continuation line.
    base_colon_break: Vec<bool>,
    /// For each declaration start, whether its declarators are chopped one per line.
    decl_chop: Vec<Option<bool>>,
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
    /// Visual column just after the delimiter.
    content_column: i32,
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
            stmt_indent: std::collections::HashMap::new(),
            expr_align: std::collections::HashMap::new(),
            pending_align: None,
            base_align: std::collections::HashMap::new(),
            pending_base: None,
            cur_line_column: 0,
            last_closed_paren_indent: None,
            preserved_line: false,
            last_code_indent: 0,
            last_code_column: 0,
            continuation_active: false,
            at_line_start: true,
            group_chop: vec![None; tokens.len()],
            group_elems: vec![0; tokens.len()],
            group_open_break: vec![false; tokens.len()],
            wrapped_inside: vec![false; tokens.len()],
            base_chop: vec![None; tokens.len()],
            base_colon_break: vec![false; tokens.len()],
            decl_chop: vec![None; tokens.len()],
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

        if newlines == 0 {
            if let Some(colon) = self.pending_base.take() {
                let space = i > 0 && !self.at_line_start && self.should_space(i);
                let column =
                    i32::try_from(self.line_len).unwrap_or(i32::MAX) + if space { 1 } else { 0 };
                self.base_align.insert(colon, column);
            }
            if let Some(owner) = self.pending_align.take() {
                let space = i > 0 && !self.at_line_start && self.should_space(i);
                let column =
                    i32::try_from(self.line_len).unwrap_or(i32::MAX) + if space { 1 } else { 0 };
                self.expr_align.insert(owner, column);
            }
        } else {
            self.pending_align = None;
        }

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
                if token.ast.stmt_start != StmtKind::None {
                    self.stmt_indent.insert(token.start, code_column);
                }
            }
        }

        if text == ":" && token.ast.colon == ColonKind::BaseList {
            self.pending_base = Some(i);
        }
        if matches!(text, "=" | "=>" | "return" | "throw")
            && token.ast.stmt_owner_start != usize::MAX
        {
            self.pending_align = Some(token.ast.stmt_owner_start);
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
        if text == ":" && token.ast.colon == ColonKind::BaseList {
            // The colon starts its own line, whether the break came from the formatter or
            // from the source layout.
            self.base_colon_break[i] = newlines > 0;
            self.base_chop[i] = Some(self.decide_base_chop(i));
        } else if text == "," && token.ast.comma == CommaKind::VariableDeclaration {
            let owner = self.layout.decl_owner[i];
            if owner != usize::MAX && self.decl_chop[owner].is_none() {
                self.decl_chop[owner] = Some(self.decide_decl_chop(i));
            }
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
            let unit = if token.ast.brace == BraceKind::NamespaceBody
                && !self.options.indent.inside_namespace
            {
                0
            } else {
                unit
            };
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
                content_column: i32::try_from(self.line_len).unwrap_or(i32::MAX),
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
        if self.options.indent.raw_literal_string == AlignIndentStyle::DoNotChange {
            return text.to_string();
        }
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
        // `align` puts the content under the opening quotes; `indent` puts it one level
        // past the statement.
        let target = match self.options.indent.raw_literal_string {
            AlignIndentStyle::Indent => {
                usize::try_from(self.last_code_indent + self.indent_unit_columns()).unwrap_or(0)
            }
            _ => self.line_len + prefix,
        };
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

    /// Whether the emitter is currently between declarations (file, type, namespace or
    /// enum body) rather than inside a method or statement block.
    fn in_declaration_body(&self) -> bool {
        matches!(
            self.stack.last().map(|frame| frame.kind),
            None | Some(BraceKind::TypeBody | BraceKind::NamespaceBody | BraceKind::EnumBody)
        )
    }

    /// Maximum number of blank lines allowed before token `i`. ReSharper keeps at most
    /// `keep_blank_lines_in_declarations` blank lines between type members and
    /// `keep_blank_lines_in_code` inside method bodies, but always removes blank lines
    /// directly next to a block brace unless `remove_blank_lines_near_braces_*` is off.
    fn blank_max_blanks(&self, i: usize) -> u32 {
        let prev = &self.tokens[i - 1];
        let pt = prev.text(self.src);
        let ct = self.tokens[i].text(self.src);
        let blank_lines = &self.options.blank_lines;
        let in_decl = self.in_declaration_body();
        let keep = if in_decl {
            blank_lines.keep_in_declarations
        } else {
            blank_lines.keep_in_code
        };
        let remove = if in_decl {
            blank_lines.remove_near_braces_in_declarations
        } else {
            blank_lines.remove_near_braces_in_code
        };
        // Blank lines between consecutive using directives are always removed.
        if self.tokens[i].ast.using_start
            && self
                .prev_code(i)
                .is_some_and(|prev| self.tokens[prev].ast.using_directive)
        {
            return 0;
        }
        // Parenthesised lists, array-rank and element-access brackets and generic
        // argument lists never contain blank lines in ReSharper's output.
        if self.inside_element_list(i) {
            return 0;
        }
        // A blank line directly inside a brace-like delimiter is removed unless the
        // matching `remove_blank_lines_near_braces_*` option is off. Collection
        // expressions and list patterns count as braces here; blank lines between their
        // elements are kept.
        if remove && matches!(pt, "{" | "[") {
            return 0;
        }
        if remove && matches!(ct, "}" | "]") {
            return 0;
        }
        keep as u32
    }

    /// Whether token `i` sits inside a parenthesised, bracketed or generic element list
    /// rather than inside a brace-delimited block.
    fn inside_element_list(&self, i: usize) -> bool {
        if self.layout.inside_generics[i] {
            return true;
        }
        let open = self.layout.enclosing_open[i];
        if open == usize::MAX {
            return false;
        }
        match self.tokens[open].text(self.src) {
            "(" => true,
            "[" => !matches!(
                self.tokens[open].ast.bracket,
                BracketKind::CollectionExpression | BracketKind::ListPattern
            ),
            _ => false,
        }
    }

    /// Maximum line breaks (blank lines plus the mandatory one) before token `i`.
    fn blank_cap_at(&self, i: usize) -> u32 {
        self.blank_max_blanks(i) + 1
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
            // `indent_preprocessor_if` / `_region` / `_other` choose between column zero,
            // the surrounding code indent, one level less, or the author's column.
            let text = token.text(self.src);
            let indent = &self.options.indent;
            let setting = if text.starts_with("#region") || text.starts_with("#endregion") {
                indent.preprocessor_region
            } else if matches!(
                text.split_whitespace().next().unwrap_or(""),
                "#if" | "#elif" | "#else" | "#endif"
            ) {
                indent.preprocessor_if
            } else {
                indent.preprocessor_other
            };
            let code = self.stack.last().map_or(0, |frame| frame.content_indent);
            return match setting {
                SpecialIndent::NoIndent => 0,
                SpecialIndent::UsualIndent => code,
                SpecialIndent::Outdent => code - unit,
                SpecialIndent::DoNotChange => {
                    let width = self.options.tab_width.max(1);
                    let columns = token.gap.tabs as usize * width + token.gap.spaces as usize;
                    i32::try_from(columns).unwrap_or(i32::MAX)
                }
            };
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
        // Continuation lines of a binary chain, a call chain or a broken list may align
        // under the expression start (`align_multiline_*`, `outdent_*`).
        if newlines > 0
            && let Some(column) = self.aligned_continuation(i, base, unit)
        {
            return column;
        }
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
            let frame = self.paren_stack.last();
            // ReSharper aligns a broken `for` header with the first token after the `(`.
            if frame.is_some_and(|frame| frame.kind == ParenKind::For) {
                return frame.map_or(base, |frame| frame.content_column);
            }
            // Lines inside a broken argument/parameter list indent one level past the line
            // that opened the delimiter, not past the enclosing block.
            let open = frame.map_or(base, |frame| frame.indent);
            let step = unit
                * i32::try_from(self.options.indent.continuous_indent_multiplier)
                    .unwrap_or(1)
                    .max(1);
            return open + step;
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
                                | ParenKind::PrimaryConstructorParameterList
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
        // A statement that is the direct body of another statement indents one level past
        // that statement, except for the `indent_nested_*_stmt` kinds.
        if newlines > 0
            && token.ast.stmt_start != StmtKind::None
            && token.ast.stmt_parent != StmtKind::None
        {
            if let Some(&parent_indent) = self.stmt_indent.get(&token.ast.stmt_parent_start) {
                // The statement after a `label:` keeps the label's indent.
                let step = if token.ast.stmt_parent == StmtKind::Labeled
                    || self.flat_nested_statement(i)
                {
                    0
                } else {
                    unit
                };
                return parent_indent + step;
            }
        }
        if newlines > 0 && token.text(self.src) == "where" && !self.options.indent.type_constraints
        {
            // `indent_type_constraints = false` keeps the constraint clause at the
            // declaration's own indent.
            return base;
        }
        if self.is_statement_label(i) {
            indent = if self.options.indent.outdent_statement_labels {
                base - unit
            } else {
                match self.options.indent_labels {
                    crate::config::LabelIndent::OneLessThanCurrent => base - unit,
                    crate::config::LabelIndent::FlushLeft => 0,
                    crate::config::LabelIndent::NoChange => base,
                }
            };
        } else if newlines > 0 && (wrapped || self.is_continuation(i)) {
            indent += self.wrap_columns();
        }
        if newlines > 0 {
            // Entries of a base list whose colon starts a continuation line sit one level
            // deeper than the colon.
            let owner = self.layout.base_owner[i];
            if owner != usize::MAX && owner != i && self.base_colon_break[owner] {
                match self.base_align.get(&owner) {
                    Some(&column) if self.options.align.multiline_extends_list => indent = column,
                    _ => indent += self.wrap_columns(),
                }
            }
        }
        if newlines > 0 && self.is_continuation(i) && self.directive_between_previous_code(i) {
            // Preprocessor directives do not interrupt a declaration: `#else` between two
            // items of a base list must not reset the continuation indent to the body level.
            indent = self.last_code_indent;
        }
        indent
    }

    /// Column of a continuation line that starts with a binary operator, a member-access
    /// dot or a list comma, when ReSharper aligns or outdents it instead of indenting it.
    fn aligned_continuation(&self, i: usize, base: i32, unit: i32) -> Option<i32> {
        let token = &self.tokens[i];
        if self.layout.inside_inline[i] {
            return None;
        }
        let text = token.text(self.src);
        let options = &self.options.align;
        let owner = token.ast.stmt_owner_start;
        let expr = self.expr_align.get(&owner).copied();
        let stmt = self.stmt_indent.get(&owner).copied().unwrap_or(base);
        if token.ast.operator == OperatorKind::Binary {
            let column = if options.multiline_binary_expressions_chain {
                expr?
            } else {
                stmt + self.wrap_columns()
            };
            return Some(if self.options.indent.outdent_binary_ops {
                column - i32::try_from(text.chars().count() + 1).unwrap_or(1)
            } else {
                column
            });
        }
        if matches!(text, "." | "?.") {
            let column = if options.multiline_calls_chain {
                expr?
            } else {
                stmt + self.wrap_columns()
            };
            return Some(if self.options.indent.outdent_dots {
                column - 1
            } else {
                column
            });
        }
        if text == "," && self.options.indent.outdent_commas {
            // A comma that starts a line moves left so the following element lines up with
            // the list content.
            let column = match self.paren_stack.last() {
                Some(paren) => paren.indent + unit,
                None => stmt + self.wrap_columns(),
            };
            return Some(column - 2);
        }
        None
    }

    /// Whether token `i` starts a statement of the same kind as its enclosing statement,
    /// for the kinds ReSharper controls with `indent_nested_*_stmt` (default: not indented).
    fn flat_nested_statement(&self, i: usize) -> bool {
        let kind = self.tokens[i].ast.stmt_start;
        if kind == StmtKind::None || kind != self.tokens[i].ast.stmt_parent {
            return false;
        }
        let indent = &self.options.indent;
        let indented = match kind {
            StmtKind::For => indent.nested_for,
            StmtKind::Foreach => indent.nested_foreach,
            StmtKind::While => indent.nested_while,
            StmtKind::Lock => indent.nested_lock,
            StmtKind::Using => indent.nested_usings,
            StmtKind::UnsafeFixed => indent.nested_fixed,
            // Other nested statements are always indented one level.
            _ => return false,
        };
        !indented
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
                .max(1)
            * i32::try_from(self.options.indent.continuous_indent_multiplier)
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
        if token.text(self.src) == "where" && !self.options.indent.type_constraints {
            // `indent_type_constraints = false` overrides the author's column.
            return None;
        }
        if self
            .aligned_continuation(i, self.last_code_indent, self.indent_unit_columns())
            .is_some()
        {
            // Aligned/outdented continuations are computed, not preserved.
            return None;
        }
        if self.options.align.multiline_extends_list {
            let owner = self.layout.base_owner[i];
            if owner != usize::MAX && owner != i && self.base_colon_break[owner] {
                // An aligned base list is recomputed from its first entry.
                return None;
            }
        }
        if self
            .paren_stack
            .last()
            .is_some_and(|paren| paren.kind == ParenKind::For)
        {
            // A broken `for` header aligns under its first token; recompute it.
            return None;
        }
        if token.ast.in_query {
            // LINQ clauses are indented from the statement, not from `from`.
            return None;
        }
        if self.options.indent.continuous_indent_multiplier > 1
            && (self.is_continuation(i) || token.ast.stmt_parent != StmtKind::None)
        {
            // A non-default `continuous_indent_multiplier` recomputes the column instead of
            // keeping the author's.
            return None;
        }
        if token.ast.stmt_parent != StmtKind::None && token.ast.stmt_start != StmtKind::None {
            // A nested statement body is indented from its parent statement, not from the
            // column the author used.
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
        // Base lists break on one side of their colon, not at their commas.
        if let Some(wrap) = self.extends_break(i) {
            return wrap;
        }
        // A type name moves down when it cannot stay on the declaration line.
        if let Some(wrap) = self.type_name_break(i) {
            return wrap;
        }
        // A constraint clause first moves as a whole.
        if let Some(wrap) = self.constraint_break(i) {
            return wrap;
        }
        // `wrap_after_…_lpar` / `wrap_before_…_rpar` placement preferences.
        if let Some(wrap) = self.lpar_break(i) {
            return wrap;
        }
        if let Some(wrap) = self.rpar_break(i) {
            return wrap;
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
                    | CommaKind::BaseList
                    | CommaKind::Constraints
                    | CommaKind::VariableDeclaration
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
                ParenKind::PrimaryConstructorParameterList => spec(
                    wrap.primary_constructor_parameters,
                    wrap.max_primary_constructor_parameters_on_line,
                    wrap.after_primary_constructor_declaration_lpar,
                    wrap.before_primary_constructor_declaration_rpar,
                ),
                // A `for` header is chopped at its semicolons, not at commas.
                ParenKind::For => spec(
                    if wrap.for_header == WrapStyle::ChopAlways {
                        WrapStyle::ChopAlways
                    } else {
                        WrapStyle::ChopIfLong
                    },
                    None,
                    false,
                    false,
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
        if let Some(max) = spec.max_on_line {
            let elements = self.group_element_count(open);
            self.group_open_break[open] = elements > max;
        }
    }

    /// Number of elements in the list delimited by `open`.
    fn group_element_count(&self, open: usize) -> usize {
        let close = self.layout.match_close[open];
        if close == usize::MAX {
            return 0;
        }
        let commas = (open + 1..close)
            .filter(|&j| {
                self.layout.enclosing_open[j] == open && self.tokens[j].text(self.src) == ","
            })
            .count();
        if self.prev_code(close) == Some(open) {
            0
        } else {
            commas + 1
        }
    }

    /// Whether the base list introduced by the `:` at `colon` is chopped one entry per line.
    fn decide_base_chop(&self, colon: usize) -> bool {
        let style = self.options.wrap.extends_list;
        if style == WrapStyle::ChopAlways {
            return true;
        }
        if style == WrapStyle::WrapIfLong {
            return false;
        }
        // A base list moved onto a line of its own, or chopped next to a colon that starts
        // its line, is measured from the colon: ReSharper keeps the entries on that line
        // when they fit and puts every entry on its own line when they do not.
        let end = self.base_list_end(colon);
        if self.base_colon_break[colon] {
            return self.too_long_between(colon + 1, end);
        }
        let multiline = (colon + 1..end).any(|j| self.tokens[j].gap.newlines > 0);
        multiline || self.too_long_between(colon + 1, end)
    }

    /// First token after the base list introduced by `colon` (its `{`, `where` or `;`).
    fn base_list_end(&self, colon: usize) -> usize {
        let mut depth = 0i32;
        for j in colon + 1..self.tokens.len() {
            let text = self.tokens[j].text(self.src);
            match text {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                "{" | "}" | ";" | "where" if depth <= 0 => return j,
                _ => {}
            }
        }
        self.tokens.len()
    }

    /// Width of the tokens `from..to` including the spaces the emitter inserts.
    fn span_width(&self, from: usize, to: usize) -> usize {
        (from..to)
            .map(|j| {
                let len = self.tokens[j].text(self.src).chars().count();
                len + usize::from(j > from && self.layout.space_before[j])
            })
            .sum()
    }

    /// Whether the declaration whose base list starts at `colon` has a primary constructor.
    fn primary_ctor(&self, colon: usize) -> bool {
        let Some(prev) = self.prev_code(colon) else {
            return false;
        };
        if self.tokens[prev].text(self.src) != ")" {
            return false;
        }
        let open = self.layout.match_close[prev];
        open != usize::MAX
            && self.tokens[open].ast.paren == ParenKind::PrimaryConstructorParameterList
    }

    /// Break right after the `(` of a call/declaration whose argument list does not fit.
    ///
    /// `wrap_after_invocation_lpar` (and its declaration counterparts) makes the opener
    /// stand alone as soon as its list has to wrap, even in fill mode.
    fn lpar_break(&self, i: usize) -> Option<bool> {
        let prev = self.prev_code(i)?;
        if prev + 1 != i || self.tokens[prev].text(self.src) != "(" {
            return None;
        }
        let spec = self.group_spec(prev)?;
        if !spec.after_lpar || self.group_chop[prev] == Some(true) || self.group_open_break[prev] {
            return None;
        }
        let close = self.layout.match_close[prev];
        if close == usize::MAX || self.layout.inside_inline[i] {
            return Some(false);
        }
        let limit = self.wrap_limit()?;
        let width = self.span_width(prev, close + 1);
        let space = usize::from(self.layout.space_before[i]);
        Some(self.line_len + space + width > limit)
    }

    /// Break before the `)` of a call/declaration whose list already wrapped.
    fn rpar_break(&self, i: usize) -> Option<bool> {
        let token = &self.tokens[i];
        if token.text(self.src) != ")" {
            return None;
        }
        let open = self.layout.match_close[i];
        if open == usize::MAX {
            return None;
        }
        let spec = self.group_spec(open)?;
        if !spec.before_rpar || self.group_chop[open] == Some(true) || self.group_open_break[open] {
            return None;
        }
        Some(self.wrapped_inside[open])
    }

    /// Break decision for the `where` that starts a type-parameter constraint clause.
    ///
    /// A constraint clause wraps as a whole: ReSharper first moves the entire `where …`
    /// run to its own line, then wraps inside it at the constraint commas.
    fn constraint_break(&self, i: usize) -> Option<bool> {
        let token = &self.tokens[i];
        if token.text(self.src) != "where" || !token.ast.has(flags::CONSTRAINT) {
            return None;
        }
        if self.layout.inside_inline[i] {
            return Some(false);
        }
        let limit = self.wrap_limit()?;
        let mut end = i;
        while end < self.tokens.len() {
            if matches!(self.tokens[end].text(self.src), "{" | "}" | ";") {
                break;
            }
            end += 1;
        }
        let width = self.span_width(i, end);
        let space = usize::from(self.layout.space_before[i]);
        Some(self.line_len + space + width > limit)
    }

    /// End of the first entry of the base list introduced by `colon` (past its comma).
    fn first_base_entry_end(&self, colon: usize) -> usize {
        let end = self.base_list_end(colon);
        (colon + 1..end)
            .find(|&j| {
                self.tokens[j].text(self.src) == ","
                    && self.tokens[j].ast.comma == CommaKind::BaseList
            })
            .map_or(end, |j| j + 1)
    }

    /// Whether `from..to` fits on the current line, counting the space before `from`.
    fn fits_on_line(&self, from: usize, to: usize) -> bool {
        let Some(limit) = self.wrap_limit() else {
            return true;
        };
        let space = usize::from(self.layout.space_before[from]);
        self.line_len + space + self.span_width(from, to) <= limit
    }

    /// Break before the declared name of a type declaration.
    ///
    /// ReSharper moves the name onto a continuation line when the name and everything that
    /// has to stay on its line does not fit. The measured run stops at the first part that
    /// wraps on its own: a primary constructor parameter list, a base list (up to its first
    /// entry) or a constraint clause.
    fn type_name_break(&self, i: usize) -> Option<bool> {
        if !self.tokens[i].ast.type_name || self.layout.inside_inline[i] {
            return None;
        }
        let mut end = i + 1;
        while end < self.tokens.len() {
            let text = self.tokens[end].text(self.src);
            if text == "("
                && self.tokens[end].ast.paren == ParenKind::PrimaryConstructorParameterList
            {
                break;
            }
            if text == ":" && self.tokens[end].ast.colon == ColonKind::BaseList {
                // In `chop_always` the first entry can move below the colon, so only the
                // name and the colon have to fit.
                end = if self.options.wrap.extends_list == WrapStyle::ChopAlways {
                    end + 1
                } else {
                    self.first_base_entry_end(end)
                };
                break;
            }
            if matches!(text, "{" | "}" | ";" | "where") {
                break;
            }
            end += 1;
        }
        Some(
            !self.fits_on_line(i, end)
                // Moving the name down only helps when the line it moves to can hold the
                // same run; a first entry that does not fit anywhere keeps the name up.
                && self.fits_at_continuation(i, end),
        )
    }

    /// Whether `from..to` fits on a continuation line.
    fn fits_at_continuation(&self, from: usize, to: usize) -> bool {
        let Some(limit) = self.wrap_limit() else {
            return true;
        };
        let base = self
            .stack
            .last()
            .map(|frame| frame.case_indent.unwrap_or(frame.content_indent))
            .unwrap_or(0);
        let width = i32::try_from(self.span_width(from, to)).unwrap_or(i32::MAX);
        let limit = i32::try_from(limit).unwrap_or(i32::MAX);
        base.saturating_add(self.wrap_columns())
            .saturating_add(width)
            <= limit
    }

    /// Break decision for the `:` of a base list and for the token following it.
    ///
    /// Besides the entry commas, a base list breaks either before its colon (the colon then
    /// starts a continuation line) or right after it. ReSharper moves the colon down for a
    /// primary constructor whose parameter list was chopped, and honours
    /// `wrap_before_extends_colon` for other declarations; the flag reverses the choice for
    /// a chopped primary constructor.
    fn extends_break(&self, i: usize) -> Option<bool> {
        let token = &self.tokens[i];
        let ct = token.text(self.src);
        let colon = if ct == ":" && token.ast.colon == ColonKind::BaseList {
            i
        } else {
            let prev = self.prev_code(i)?;
            let pt = self.tokens[prev].text(self.src);
            if pt != ":" || self.tokens[prev].ast.colon != ColonKind::BaseList {
                return None;
            }
            prev
        };
        if self.layout.inside_inline[i] {
            return Some(false);
        }
        let style = self.options.wrap.extends_list;
        let before_extends_colon = self.options.wrap.before_extends_colon;
        if ct == ":" {
            let primary = self.primary_ctor(colon);
            return Some(match style {
                WrapStyle::ChopAlways => {
                    primary
                        && !self.base_colon_break[colon]
                        && !self.fits_on_line(colon, self.first_base_entry_end(colon))
                }
                _ => {
                    // Moving the colon to its own line only helps when the whole list fits
                    // there; otherwise the list keeps wrapping at its commas.
                    let move_colon = if primary {
                        if before_extends_colon {
                            // The flag reverses the choice for a chopped primary constructor:
                            // the colon only moves when the first entry does not fit.
                            !self.fits_on_line(colon, self.first_base_entry_end(colon))
                        } else {
                            !self.fits_on_line(colon, self.base_list_end(colon))
                        }
                    } else {
                        before_extends_colon && !self.fits_on_line(colon, self.base_list_end(colon))
                    };
                    move_colon && !self.fits_on_line(colon, self.base_list_end(colon))
                }
            });
        }
        if self.base_colon_break[colon] {
            return Some(false);
        }
        let first_end = self.first_base_entry_end(colon);
        Some(match style {
            WrapStyle::ChopAlways => !self.fits_on_line(i, first_end),
            WrapStyle::ChopIfLong => {
                self.base_chop[colon] == Some(true) && !self.fits_on_line(i, first_end)
            }
            WrapStyle::WrapIfLong => {
                // Breaking after the colon only helps when the base list still overflows a
                // line of its own; otherwise ReSharper keeps it in place and wraps at the
                // commas.
                let Some(limit) = self.wrap_limit() else {
                    return Some(false);
                };
                let base = self
                    .stack
                    .last()
                    .map(|frame| frame.case_indent.unwrap_or(frame.content_indent))
                    .unwrap_or(0);
                let limit = i32::try_from(limit).unwrap_or(i32::MAX);
                let width = i32::try_from(self.span_width(colon, self.base_list_end(colon)))
                    .unwrap_or(i32::MAX);
                let fits_alone = base
                    .saturating_add(self.wrap_columns())
                    .saturating_add(width)
                    <= limit;
                !fits_alone && !before_extends_colon
            }
        })
    }

    /// Whether the tokens `from..to` still fit on the current line.
    fn too_long_between(&self, from: usize, to: usize) -> bool {
        let Some(limit) = self.wrap_limit() else {
            return false;
        };
        let width: usize = (from..to)
            .map(|j| {
                let len = self.tokens[j].text(self.src).chars().count();
                len + usize::from(self.layout.space_before[j])
            })
            .sum();
        self.line_len + width > limit
    }

    /// Whether the declarators of the declaration containing `comma` are chopped.
    fn decide_decl_chop(&self, comma: usize) -> bool {
        let style = self.options.wrap.multiple_declaration;
        if style == WrapStyle::ChopAlways {
            return true;
        }
        let Some(end) = self.declaration_end(comma) else {
            return false;
        };
        let too_long = self.too_long_between(comma + 1, end + 1);
        if style == WrapStyle::WrapIfLong {
            return too_long;
        }
        let multiline = (comma + 1..end).any(|j| self.tokens[j].gap.newlines > 0);
        multiline || too_long
    }

    /// Index of the `;` that ends the declaration containing `comma`.
    fn declaration_end(&self, comma: usize) -> Option<usize> {
        let mut depth = 0i32;
        for j in comma + 1..self.tokens.len() {
            let text = self.tokens[j].text(self.src);
            match text {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                ";" if depth == 0 => return Some(j),
                _ => {}
            }
        }
        None
    }

    /// Forced break points inside a chopped group or a max-elements-per-line group.
    fn group_break(&self, i: usize) -> bool {
        if self.chop_break(i) {
            return true;
        }
        let open = self.layout.enclosing_open[i];
        if open == usize::MAX || self.layout.inside_inline[i] {
            return false;
        }
        let Some(spec) = self.group_spec(open) else {
            return false;
        };
        let text = self.tokens[i].text(self.src);
        let chopped = self.group_chop[open] == Some(true);
        // A `for` header is chopped at its semicolons; its commas stay put.
        let is_for = self.tokens[open].ast.paren == ParenKind::For;
        if chopped || self.group_open_break[open] {
            if i == self.layout.match_close[open] && spec.before_rpar {
                return true;
            }
            if self.prev_code(i) == Some(open) && spec.after_lpar {
                return true;
            }
        }
        let prev = self.prev_code(i);
        if let Some(prev) = prev {
            if self.tokens[prev].text(self.src) == ";" && self.layout.enclosing_open[prev] == open {
                if chopped && is_for {
                    return true;
                }
            }
        }
        if chopped && !is_for {
            if text == "," && spec.before_comma {
                return true;
            }
            if let Some(prev) = prev {
                if !spec.before_comma
                    && self.tokens[prev].text(self.src) == ","
                    && self.layout.enclosing_open[prev] == open
                {
                    return true;
                }
            }
        }
        if let (Some(prev), Some(max)) = (prev, spec.max_on_line) {
            if self.tokens[prev].text(self.src) == ","
                && self.layout.enclosing_open[prev] == open
                && self.group_elems[open] >= max
            {
                return true;
            }
        }
        false
    }

    /// Forced break points of a chopped base list or multi-declarator declaration.
    fn chop_break(&self, i: usize) -> bool {
        if self.layout.inside_inline[i] {
            return false;
        }
        let before_comma = self.options.wrap.before_comma;
        if before_comma {
            if self.tokens[i].text(self.src) != "," {
                return false;
            }
            let kind = self.tokens[i].ast.comma;
            if kind == CommaKind::BaseList {
                let owner = self.layout.base_owner[i];
                if owner != usize::MAX && self.base_chop[owner] == Some(true) {
                    return true;
                }
            } else if kind == CommaKind::VariableDeclaration {
                let owner = self.layout.decl_owner[i];
                if owner != usize::MAX && self.decl_chop[owner] == Some(true) {
                    return true;
                }
            }
            return false;
        }
        let Some(prev) = self.prev_code(i) else {
            return false;
        };
        if self.tokens[prev].text(self.src) != "," {
            return false;
        }
        match self.tokens[prev].ast.comma {
            CommaKind::BaseList => {
                let owner = self.layout.base_owner[prev];
                owner != usize::MAX && self.base_chop[owner] == Some(true)
            }
            CommaKind::VariableDeclaration => {
                let owner = self.layout.decl_owner[prev];
                owner != usize::MAX && self.decl_chop[owner] == Some(true)
            }
            _ => false,
        }
    }

    /// Reset per-line element counters for every group a new line starts inside, and
    /// count the element that starts at token `i`.
    fn update_group_counts(&mut self, i: usize, newlines: u32) {
        if newlines > 0 {
            let mut open = self.layout.enclosing_open[i];
            while open != usize::MAX {
                self.group_elems[open] = 0;
                self.wrapped_inside[open] = true;
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

    /// Line breaks before token `i`: the structural layout decision from
    /// [`Self::base_newlines_before`] clamped to ReSharper's blank-line limits.
    ///
    /// ReSharper's `BlankLineStage` computes `min` and `max` blank-line counts for every gap
    /// (from the option rules and the enclosing body) and clamps the gap to
    /// `[min, max]`; a minimum wins over a smaller maximum.
    fn newlines_before(&self, i: usize) -> u32 {
        let base = self.base_newlines_before(i);
        if base == 0 {
            return 0;
        }
        let (min, max) = self.blank_bounds(i);
        let max = max.max(min);
        base.max(min + 1).min(max + 1)
    }

    /// `(min, max)` blank lines ReSharper allows before token `i`.
    fn blank_bounds(&self, i: usize) -> (u32, u32) {
        let token = &self.tokens[i];
        let prev = &self.tokens[i - 1];
        let pt = prev.text(self.src);
        let ct = token.text(self.src);
        let blank_lines = &self.options.blank_lines;
        let max = self.blank_max_blanks(i);

        // Region padding: `around_region` is a minimum, `inside_region` is exact.
        let after_region_start =
            prev.kind == TokenKind::Directive && (pt == "#region" || pt.starts_with("#region "));
        if ct.starts_with("#endregion") || after_region_start {
            let inside = blank_lines.inside_region as u32;
            return (inside, inside);
        }
        if ct.starts_with("#region") || (prev.kind == TokenKind::Directive && pt == "#endregion") {
            let around = blank_lines.around_region as u32;
            return (around, max.max(around));
        }

        // Padding inside a type or namespace body is its own option, independent of the
        // near-brace removal.
        if self.in_declaration_body() && (pt == "{" || ct == "}") {
            let inside = match self.stack.last().map(|frame| frame.kind) {
                Some(BraceKind::NamespaceBody) => blank_lines.inside_namespace as u32,
                Some(BraceKind::TypeBody | BraceKind::EnumBody) => blank_lines.inside_type as u32,
                _ => 0,
            };
            if inside > 0 {
                return (inside, max.max(inside));
            }
        }

        // `CSharpFormattingStage.VisitUsingList`: consecutive `using` directives are
        // written with exactly one line break, or `blank_lines_between_using_groups + 1`
        // when they fall into different groups.
        if token.ast.using_start {
            if let Some(prev_code) = self.prev_code(i) {
                if self.tokens[prev_code].text(self.src) == ";"
                    && self.tokens[prev_code].ast.using_directive
                {
                    let blank = self.using_group_blanks(prev_code, i);
                    return (blank, blank);
                }
            }
        }
        // `blank_lines_after_file_scoped_namespace_directive` is exact.
        if prev.text(self.src) == ";" && prev.ast.decl_end == DeclKind::Namespace {
            let after = blank_lines.after_file_scoped_namespace_directive as u32;
            return (after, after);
        }

        // No blank line is inserted directly next to a block brace.
        if pt == "{" || ct == "}" {
            return (0, max);
        }
        (self.blank_min_blanks(i), max)
    }

    /// Blank lines ReSharper puts between two `using` directives: none inside a group,
    /// `blank_lines_between_using_groups` otherwise.
    fn using_group_blanks(&self, prev_end: usize, next_start: usize) -> u32 {
        let groups = self.options.blank_lines.between_using_groups as u32;
        match (self.using_key(prev_end), self.using_key(next_start)) {
            (Some(left), Some(right)) => {
                // Aliases group with each other; symbol directives group when their
                // `static` flag matches and they share the first namespace segment.
                let same_group = if left.0 && right.0 {
                    true
                } else if !left.0 && !right.0 {
                    left.1 == right.1 && left.2 == right.2
                } else {
                    false
                };
                if same_group { 0 } else { groups }
            }
            _ => 0,
        }
    }

    /// `(is_alias, is_static, first segment of the imported name)` for the `using`
    /// directive that contains token `pos`.
    fn using_key(&self, pos: usize) -> Option<(bool, bool, String)> {
        let mut start = pos;
        loop {
            if self.tokens[start].ast.using_start {
                break;
            }
            if start == 0 {
                return None;
            }
            start -= 1;
        }
        let mut end = start;
        while end + 1 < self.tokens.len() {
            end += 1;
            if self.tokens[end].text(self.src) == ";" {
                break;
            }
        }
        let mut alias = false;
        let mut is_static = false;
        let mut first = String::new();
        let mut seen_name = false;
        for k in start + 1..=end {
            let token = &self.tokens[k];
            if token.kind.is_comment() {
                continue;
            }
            let text = token.text(self.src);
            match text {
                "using" | "global" => {}
                "static" => is_static = true,
                "=" => alias = true,
                ";" => break,
                _ => {
                    if !seen_name {
                        seen_name = true;
                        first.push_str(text);
                    }
                }
            }
        }
        Some((alias, is_static, first))
    }

    /// Minimum number of blank lines ReSharper inserts before token `i`.
    ///
    /// Mirrors the `BlankLinesAroundNodeRule` registrations in
    /// `CSharpFormatterInfoProviderBase.BlankLines()`: every member kind, statement kind and
    /// switch section carries a `blank_lines_*` option that forces blank lines around it.
    fn blank_min_blanks(&self, i: usize) -> u32 {
        let token = &self.tokens[i];
        let prev = &self.tokens[i - 1];
        let ct = token.text(self.src);
        let blank_lines = &self.options.blank_lines;
        let mut min = 0u32;

        // Members and other declaration units: `blank_lines_around_*`. A member whose
        // leading comment sits on its own line counts as multi-line, and the blank line
        // goes above the comment (see the comment branch below).
        if token.ast.decl_start != DeclKind::None
            && !prev.kind.is_comment()
            && prev.kind != TokenKind::Directive
        {
            // A member whose attributes move onto their own line is multi-line even when
            // the source kept it on one line.
            let multiline = self.member_multiline(i)
                || token.ast.decl_start == DeclKind::Type
                || (ct == "["
                    && token.ast.member_attribute
                    && (token.ast.attribute == AttributePlacement::OwnLine
                        || self.attribute_list_long(i)));
            min = min.max(self.around_decl_min(token.ast.decl_start, multiline));
        }
        if token.kind.is_comment() {
            if let Some(next) = self.next_code(i) {
                let decl = &self.tokens[next];
                if decl.ast.member_start
                    && decl.ast.decl_start != DeclKind::None
                    && self.declaration_comment_start(next) == Some(i)
                {
                    min = min.max(self.around_decl_min(decl.ast.decl_start, true));
                }
            }
        }
        if prev.ast.decl_end != DeclKind::None && token.kind != TokenKind::Directive {
            min = min.max(self.around_decl_min(prev.ast.decl_end, self.decl_end_multiline(i - 1)));
        }

        // Statements that own a block, span several lines, or transfer control.
        // A bare block statement is not a "block statement" for ReSharper: it gets no
        // separating blank lines of its own.
        if token.ast.stmt_start != StmtKind::None && token.ast.stmt_start != StmtKind::Block {
            let kind = token.ast.stmt_start;
            if kind == StmtKind::Switch || token.ast.stmt_start_has_block {
                min = min.max(blank_lines.before_block_statements as u32);
            }
            if token.ast.stmt_start_multiline || token.ast.stmt_start_has_block {
                min = min.max(blank_lines.before_multiline_statements as u32);
            }
            if token.ast.stmt_start_control != 0 && self.separate_before(i) {
                min = min.max(blank_lines.before_control_transfer_statements as u32);
            }
        }
        if prev.ast.stmt_end != StmtKind::None && prev.ast.stmt_end != StmtKind::Block {
            let kind = prev.ast.stmt_end;
            if kind == StmtKind::Switch || prev.ast.stmt_end_has_block {
                min = min.max(blank_lines.after_block_statements as u32);
            }
            if prev.ast.stmt_end_multiline || prev.ast.stmt_end_has_block {
                min = min.max(blank_lines.after_multiline_statements as u32);
            }
            if prev.ast.stmt_end_control != 0 && self.separate_after(i) {
                min = min.max(blank_lines.after_control_transfer_statements as u32);
            }
        }

        // Switch sections: blank lines before/after the labels and around whole sections.
        if matches!(ct, "case" | "default") && token.ast.switch_section {
            min = min.max(blank_lines.before_case as u32);
            if !token.ast.switch_section_inline {
                min = min.max(blank_lines.around_multiline_case_section as u32);
            }
            if token.ast.switch_section_block_first {
                min = min.max(blank_lines.around_block_case_section as u32);
            }
        }
        if prev.ast.switch_section && !token.ast.switch_section {
            if !prev.ast.switch_section_inline {
                min = min.max(blank_lines.around_multiline_case_section as u32);
            }
            if prev.ast.switch_section_block_last {
                min = min.max(blank_lines.around_block_case_section as u32);
            }
        }
        if prev.ast.colon == ColonKind::SwitchLabel
            && !self
                .next_code(i)
                .is_some_and(|next| matches!(self.tokens[next].text(self.src), "case" | "default"))
        {
            min = min.max(blank_lines.after_case as u32);
        }

        // Blank lines the author left above a single-line comment.
        if token.kind == TokenKind::LineComment
            && !self.tokens[i].text(self.src).starts_with("////")
        {
            min = min.max(blank_lines.before_single_line_comment as u32);
        }

        // The first code token after the file's leading comment block.
        if prev.kind.is_comment() && self.leading_comment_run(i - 1) {
            min = min.max(blank_lines.after_start_comment as u32);
        }

        // The first non-using token after the last `using` of a using list.
        if prev.ast.using_end && ct != "#endif" {
            min = min.max(blank_lines.after_using_list as u32);
        }

        min
    }

    /// Blank lines forced by `blank_lines_around_*` for a declaration of `kind`.
    fn around_decl_min(&self, kind: DeclKind, multiline: bool) -> u32 {
        let blank_lines = &self.options.blank_lines;
        let value = match kind {
            DeclKind::None => return 0,
            DeclKind::Namespace => blank_lines.around_namespace,
            DeclKind::Type => {
                if multiline {
                    blank_lines.around_type
                } else {
                    blank_lines.around_single_line_type
                }
            }
            DeclKind::Field => {
                if multiline {
                    blank_lines.around_field
                } else {
                    blank_lines.around_single_line_field
                }
            }
            DeclKind::Property => {
                if multiline {
                    blank_lines.around_property
                } else {
                    blank_lines.around_single_line_property
                }
            }
            DeclKind::AutoProperty => {
                if multiline {
                    blank_lines.around_auto_property
                } else {
                    blank_lines.around_single_line_auto_property
                }
            }
            DeclKind::Accessor => {
                if multiline {
                    blank_lines.around_accessor
                } else {
                    blank_lines.around_single_line_accessor
                }
            }
            DeclKind::Invocable => {
                if multiline {
                    blank_lines.around_invocable
                } else {
                    blank_lines.around_single_line_invocable
                }
            }
            DeclKind::LocalMethod => {
                if multiline {
                    blank_lines.around_local_method
                } else {
                    blank_lines.around_single_line_local_method
                }
            }
        };
        value as u32
    }

    /// Whether the declaration ending at token `j` spans several lines after formatting.
    fn decl_end_multiline(&self, j: usize) -> bool {
        let token = &self.tokens[j];
        if token.text(self.src) == "}" {
            return !self.layout.inline_close(j);
        }
        token.ast.decl_end_multiline
    }

    /// The first token of the comment run that leads the declaration starting at `i`, when
    /// that comment starts its own line.
    fn declaration_comment_start(&self, i: usize) -> Option<usize> {
        if i == 0 || !self.tokens[i - 1].kind.is_comment() {
            return None;
        }
        let mut start = i - 1;
        while start > 0
            && self.tokens[start - 1].kind.is_comment()
            && (start - 1 == 0 || self.tokens[start - 1].gap.newlines > 0)
            && self.tokens[start].gap.newlines > 0
        {
            start -= 1;
        }
        if start == 0 || self.tokens[start].gap.newlines > 0 {
            Some(start)
        } else {
            None
        }
    }

    /// Whether token `i-1` closes the file's leading comment block.
    fn leading_comment_run(&self, j: usize) -> bool {
        self.tokens[..=j]
            .iter()
            .all(|token| token.kind.is_comment())
    }

    /// `AdditionalCheckForBlankLineBefore` of `BlankLinesAroundControlTransferStatement`.
    fn separate_before(&self, i: usize) -> bool {
        let current = (
            self.tokens[i].ast.stmt_start_control,
            self.tokens[i].ast.stmt_start_control_level,
        );
        let prev = &self.tokens[i - 1];
        if prev.ast.stmt_end == StmtKind::None {
            return current.0 != 0;
        }
        let previous = (prev.ast.stmt_end_control, prev.ast.stmt_end_control_level);
        should_separate(current, previous)
    }

    /// `AdditionalCheckForBlankLineAfter` of `BlankLinesAroundControlTransferStatement`.
    fn separate_after(&self, i: usize) -> bool {
        let prev = &self.tokens[i - 1];
        let current = (prev.ast.stmt_end_control, prev.ast.stmt_end_control_level);
        let token = &self.tokens[i];
        if token.ast.stmt_start == StmtKind::None {
            return current.0 != 0;
        }
        let next = (
            token.ast.stmt_start_control,
            token.ast.stmt_start_control_level,
        );
        should_separate(current, next)
    }

    fn base_newlines_before(&self, i: usize) -> u32 {
        let token = &self.tokens[i];
        let prev = &self.tokens[i - 1];
        let pt = prev.text(self.src);
        let ct = token.text(self.src);
        let gap = token.gap.newlines;
        let in_inline = self.layout.inside_inline[i];
        let cap = self.blank_cap_at(i);

        // Region directives: `around_region` is the blank line before `#region`,
        // `inside_region` the padding just after `#region` and before `#endregion`.
        if ct.starts_with("#region") && pt != "{" {
            return (self.options.blank_lines.around_region as u32 + 1).min(cap);
        }
        if ct.starts_with("#endregion") {
            return (self.options.blank_lines.inside_region as u32 + 1).min(cap);
        }
        if prev.kind == TokenKind::Directive && (pt == "#region" || pt.starts_with("#region ")) {
            return (self.options.blank_lines.inside_region as u32 + 1).min(cap);
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
            // The default separation comes from the `blank_lines_around_*` rules applied
            // by `blank_min_blanks`; `blank_lines_between_members` overrides it.
            let blank = self
                .options
                .blank_lines
                .between_members
                .map_or(0, |b| b as u32);
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

        // Blanks just inside a brace are dropped by `blank_cap_at` (unless
        // `remove_blank_lines_near_braces_*` is off), so the author's gap is kept up to
        // the cap here.
        if ct == "}" {
            if in_inline {
                return gap;
            }
            if pt == "{" {
                return gap.max(1).min(cap);
            }
            return gap.max(1).min(cap);
        }

        if pt == "{" {
            if in_inline {
                return gap;
            }
            if token.kind == TokenKind::BlockComment && gap == 0 {
                return 0;
            }
            return gap.max(1).min(cap);
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
            // Separating blank lines are supplied by `blank_min_blanks`; keep the
            // author's gap otherwise.
            return gap.max(1).min(cap);
        }

        if let Some(blank) = self.options.blank_lines.between_members {
            if self.is_member_start(i) && matches!(pt, "}" | ";") && !matches!(ct, "}" | ";") {
                return (blank as u32 + 1).min(cap);
            }
        }

        if pt == "#region" || pt.starts_with("#region ") {
            return (self.options.blank_lines.inside_region as u32 + 1).min(cap);
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
fn rsharp_space(options: &FormatOptions, key: &str, default: bool) -> bool {
    options.space.resharper.get(key).copied().unwrap_or(default)
}

/// The granular `resharper_space_around_*_op` override for an operator, if set.
fn binary_operator_space(options: &FormatOptions, operator: &str) -> Option<bool> {
    let key = match operator {
        "+" | "-" => "around_additive_op",
        "*" | "/" | "%" => "around_multiplicative_op",
        "<<" | ">>" | ">>>" => "around_shift_op",
        "<" | ">" | "<=" | ">=" => "around_relational_op",
        "==" | "!=" => "around_equality_op",
        "&" | "|" | "^" => "around_bitwise_op",
        "&&" | "||" => "around_logical_op",
        "??" => "around_nullcoalescing_op",
        "=>" => "around_lambda_arrow",
        _ if is_assignment_operator(operator) => "around_assignment_op",
        _ => return None,
    };
    options.space.resharper.get(key).copied()
}

fn before_paren_space(options: &FormatOptions, kind: ParenKind, default: bool) -> bool {
    let key = match kind {
        ParenKind::If => "before_if_parentheses",
        ParenKind::While => "before_while_parentheses",
        ParenKind::For => "before_for_parentheses",
        ParenKind::ForEach => "before_foreach_parentheses",
        ParenKind::Switch => "before_switch_parentheses",
        ParenKind::Catch => "before_catch_parentheses",
        ParenKind::Using => "before_using_parentheses",
        ParenKind::Lock => "before_lock_parentheses",
        ParenKind::Fixed => "before_fixed_parentheses",
        ParenKind::Checked => "before_checked_parentheses",
        ParenKind::TypeOf => "before_typeof_parentheses",
        ParenKind::SizeOf => "before_sizeof_parentheses",
        ParenKind::Default => "before_default_parentheses",
        ParenKind::ArgumentList | ParenKind::AttributeArguments => "before_method_call_parentheses",
        ParenKind::ParameterList
        | ParenKind::BracketedParameterList
        | ParenKind::PrimaryConstructorParameterList => "before_method_parentheses",
        _ => return default,
    };
    rsharp_space(options, key, default)
}

fn within_paren_space(options: &FormatOptions, kind: ParenKind, default: bool) -> bool {
    let key = match kind {
        ParenKind::If => "within_if_parentheses",
        ParenKind::While => "within_while_parentheses",
        ParenKind::For => "within_for_parentheses",
        ParenKind::ForEach => "within_foreach_parentheses",
        ParenKind::Switch => "within_switch_parentheses",
        ParenKind::Catch => "within_catch_parentheses",
        ParenKind::Using => "within_using_parentheses",
        ParenKind::Lock => "within_lock_parentheses",
        ParenKind::Fixed => "within_fixed_parentheses",
        ParenKind::Checked => "within_checked_parentheses",
        ParenKind::TypeOf => "within_typeof_parentheses",
        ParenKind::SizeOf => "within_sizeof_parentheses",
        ParenKind::Default => "within_default_parentheses",
        ParenKind::ArgumentList
        | ParenKind::AttributeArguments
        | ParenKind::ParameterList
        | ParenKind::BracketedParameterList
        | ParenKind::PrimaryConstructorParameterList => return default,
        ParenKind::Parenthesized | ParenKind::Tuple | ParenKind::Cast => "within_parentheses",
        _ => return default,
    };
    rsharp_space(options, key, default)
}

/// Roslyn fallback for the space inside a parenthesis group.
fn paren_within_default(options: &FormatOptions, kind: ParenKind) -> bool {
    match kind {
        ParenKind::ArgumentList | ParenKind::AttributeArguments => {
            options.space.method_call_parameter_list_parentheses
        }
        ParenKind::ParameterList
        | ParenKind::BracketedParameterList
        | ParenKind::PrimaryConstructorParameterList => {
            options.space.method_declaration_parameter_list_parentheses
        }
        _ => options.space.between_parentheses,
    }
}

fn within_bracket_space(options: &FormatOptions, kind: BracketKind, default: bool) -> bool {
    let key = match kind {
        BracketKind::ElementAccess => "within_array_access_brackets",
        BracketKind::ArrayRank => "within_array_rank_brackets",
        BracketKind::Attribute => "within_attribute_brackets",
        BracketKind::CollectionExpression | BracketKind::ListPattern => {
            "within_list_pattern_brackets"
        }
        _ => return default,
    };
    rsharp_space(options, key, default)
}

/// Space after a prefix unary operator (`!x`, `-x`, `&x`, `*x`, `++x`).
fn unary_after_space(options: &FormatOptions, operator: &str) -> bool {
    let fallback = rsharp_space(options, "after_unary_operator", false);
    let key = match operator {
        "!" => "after_logical_not_op",
        "-" => "after_unary_minus_op",
        "+" => "after_unary_plus_op",
        "&" => "after_ampersand_op",
        "*" => "after_asterik_op",
        "++" | "--" => return rsharp_space(options, "near_postfix_and_prefix_op", fallback),
        _ => return fallback,
    };
    rsharp_space(options, key, fallback)
}

/// `using X = Y;` uses its own spacing preference for the `=`.
fn alias_eq_before(src: &str, tokens: &[Token], eq: usize) -> bool {
    eq >= 2 && tokens[eq - 2].text(src) == "using"
}

fn is_accessor_keyword(text: &str) -> bool {
    matches!(text, "get" | "set" | "init" | "add" | "remove")
}

fn in_accessor_list(layout: &Layout, tokens: &[Token], i: usize) -> bool {
    let open = layout.enclosing_open[i];
    open != usize::MAX && tokens[open].ast.brace == BraceKind::AccessorList
}

/// Space between a brace and the token on the other side of it.
fn brace_space_inside(
    tokens: &[Token],
    layout: &Layout,
    options: &FormatOptions,
    open: usize,
) -> bool {
    let close = layout.brace_match[open];
    let empty = close != usize::MAX
        && (close == open + 1 || tokens[open + 1..close].iter().all(Token::is_trivia));
    if empty {
        return rsharp_space(options, "within_empty_braces", true);
    }
    let key = match (tokens[open].ast.brace, tokens[open].ast.brace_owner) {
        (BraceKind::AccessorList, _) | (_, BraceOwner::Accessor) => "in_singleline_accessorholder",
        (_, BraceOwner::Lambda | BraceOwner::AnonymousMethod) => "in_singleline_anonymous_method",
        (
            _,
            BraceOwner::Method
            | BraceOwner::Constructor
            | BraceOwner::Destructor
            | BraceOwner::Operator
            | BraceOwner::LocalFunction,
        ) => "in_singleline_method",
        (
            BraceKind::Initializer
            | BraceKind::Pattern
            | BraceKind::SwitchExpression
            | BraceKind::Other
            | BraceKind::None,
            _,
        )
        | (_, BraceOwner::AnonymousType | BraceOwner::Initializer) => {
            "within_single_line_array_initializer_braces"
        }
        _ => return true,
    };
    rsharp_space(options, key, true)
}

/// Space between a keyword and a parenthesized tuple/expression that follows it.
fn keyword_paren_space(
    src: &str,
    tokens: &[Token],
    layout: &Layout,
    options: &FormatOptions,
    i: usize,
) -> bool {
    let keyword = tokens[i - 1].text(src);
    if matches!(
        keyword,
        "as" | "public"
            | "private"
            | "protected"
            | "internal"
            | "static"
            | "virtual"
            | "override"
            | "abstract"
            | "sealed"
            | "extern"
            | "unsafe"
            | "readonly"
            | "volatile"
            | "partial"
            | "async"
            | "const"
    ) {
        return rsharp_space(options, "between_keyword_and_type", true);
    }
    // `ref (int, int) x` declares a tuple type; `return (1, 2)` yields a tuple value.
    let close = layout.match_close[i];
    if close != usize::MAX && close + 1 < tokens.len() && is_word(&tokens[close + 1]) {
        return rsharp_space(options, "between_keyword_and_type", true);
    }
    rsharp_space(options, "between_keyword_and_expression", true)
}

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
        if cur.kind == TokenKind::LineComment && cur.gap.newlines == 0 {
            return rsharp_space(options, "before_trailing_comment", true);
        }
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

    // `..` keeps the author's spacing except inside a list pattern, which has its own
    // preference (`within_slice_pattern`).
    if pt == ".." {
        let open = layout.enclosing_open[i - 1];
        if open != usize::MAX && tokens[open].ast.bracket == BracketKind::ListPattern {
            return rsharp_space(options, "within_slice_pattern", true);
        }
        return cur.gap.spaces > 0 || cur.gap.tabs > 0;
    }
    if ct == ".." {
        return cur.gap.spaces > 0 || cur.gap.tabs > 0;
    }

    if ct == "{" {
        if cur.ast.brace == BraceKind::AccessorList {
            return rsharp_space(options, "before_singleline_accessorholder", true);
        }
        if pt == "=>" {
            // `x => { … }`: the brace inherits the lambda arrow's spacing.
            return binary_operator_space(options, pt).unwrap_or(true);
        }
        return true;
    }
    if pt == "{" {
        return brace_space_inside(tokens, layout, options, i - 1);
    }
    if ct == "}" {
        let open = layout.brace_match[i];
        if open != usize::MAX {
            return brace_space_inside(tokens, layout, options, open);
        }
        return true;
    }
    if pt == ";" && is_accessor_keyword(ct) && in_accessor_list(layout, tokens, i) {
        return rsharp_space(options, "between_accessors_in_singleline_property", true);
    }
    if pt == "}" {
        if matches!(ct, "?" | ":") {
            // Ternary/question and case-label colons are decided by the operator rules below.
        } else if is_accessor_keyword(ct) && in_accessor_list(layout, tokens, i) {
            return rsharp_space(options, "between_accessors_in_singleline_property", true);
        } else if ct == ";" {
            return rsharp_space(options, "before_semicolon", false);
        } else {
            return !matches!(ct, "," | ")" | "]" | "." | "?." | "!");
        }
    }

    if layout.generic_open[i] {
        let key = if tokens[i].ast.angle == AngleKind::TypeParameterOpen {
            "before_type_parameter_angle"
        } else {
            "before_type_argument_angle"
        };
        return rsharp_space(options, key, false);
    }
    if layout.generic_close[i] {
        let key = if tokens[i].ast.angle == AngleKind::TypeParameterClose {
            "within_type_parameter_angles"
        } else {
            "within_type_argument_angles"
        };
        return rsharp_space(options, key, false);
    }
    if layout.generic_open[i - 1] {
        let key = if tokens[i - 1].ast.angle == AngleKind::TypeParameterOpen {
            "within_type_parameter_angles"
        } else {
            "within_type_argument_angles"
        };
        return rsharp_space(options, key, false);
    }
    if layout.generic_close[i - 1] {
        if ct == ";" {
            return rsharp_space(options, "before_semicolon", false);
        }
        return !matches!(
            ct,
            "(" | ")" | "]" | "[" | "," | "." | "?." | "?" | "!" | "->"
        );
    }

    if matches!(pt, "." | "->" | "::" | "?." | "?[")
        || matches!(ct, "." | "->" | "::" | "?." | "?[")
    {
        if pt == "." || ct == "." {
            return rsharp_space(options, "around_dot", false);
        }
        if pt == "->" || ct == "->" {
            return rsharp_space(options, "around_arrow_op", false);
        }
        return false;
    }

    if (ct == "=" || pt == "=") && alias_eq_before(src, tokens, if ct == "=" { i } else { i - 1 }) {
        return rsharp_space(options, "around_alias_eq", true);
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
        if is_accessor_keyword(ct) && in_accessor_list(layout, tokens, i) {
            return rsharp_space(options, "between_accessors_in_singleline_property", true);
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
        if cur.ast.semicolon == SemicolonKind::ForStatement {
            return options.space.before_semicolon_in_for;
        }
        return rsharp_space(options, "before_semicolon", false);
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
            return rsharp_space(options, "between_typecast_parentheses", false);
        }
        if i >= 2 && tokens[i - 2].text(src) == "nameof" {
            return rsharp_space(options, "within_nameof_parentheses", false);
        }
        if i >= 2 && tokens[i - 2].text(src) == "new" {
            return rsharp_space(options, "within_new_parentheses", false);
        }
        let kind = prev.ast.paren;
        return within_paren_space(options, kind, paren_within_default(options, kind));
    }
    // `(T)x` — no space between the cast and its operand unless configured.
    if pt == ")" && prev.ast.paren == ParenKind::Cast {
        return options.space.after_cast;
    }
    // `(int, int)Method2()` — a cast is glued to the operand that follows it. A return-type
    // tuple in a declaration keeps its space because no expression starter precedes it.
    if pt == ")"
        && cur.kind == TokenKind::Ident
        && tokens.get(i + 1).is_some_and(|t| t.text(src) == "(")
        && paren_is_cast_like(src, tokens, layout, i - 1)
    {
        return options.space.after_cast;
    }
    if ct == ")" {
        if pt == "(" {
            return empty_paren_space(prev, options);
        }
        let open = layout.enclosing_open[i];
        if open != usize::MAX && open > 0 {
            let before_open = tokens[open - 1].text(src);
            if before_open == "nameof" {
                return rsharp_space(options, "within_nameof_parentheses", false);
            }
            if before_open == "new" {
                return rsharp_space(options, "within_new_parentheses", false);
            }
        }
        let kind = if open != usize::MAX {
            tokens[open].ast.paren
        } else {
            ParenKind::None
        };
        return within_paren_space(options, kind, paren_within_default(options, kind));
    }

    if pt == "[" {
        if ct == "]" {
            let key = match prev.ast.bracket {
                BracketKind::ArrayRank => "within_array_rank_empty_brackets",
                BracketKind::ElementAccess => "within_array_access_brackets",
                _ => return options.space.between_empty_square_brackets,
            };
            return rsharp_space(options, key, options.space.between_empty_square_brackets);
        }
        return within_bracket_space(
            options,
            prev.ast.bracket,
            options.space.between_square_brackets,
        );
    }
    if ct == "]" {
        if pt == "[" {
            return options.space.between_empty_square_brackets;
        }
        let open = layout.enclosing_open[i];
        let kind = if open != usize::MAX {
            tokens[open].ast.bracket
        } else {
            BracketKind::None
        };
        return within_bracket_space(options, kind, options.space.between_square_brackets);
    }
    if pt == "]" && prev.ast.bracket == BracketKind::Attribute {
        if cur.ast.bracket == BracketKind::Attribute {
            return rsharp_space(options, "between_attribute_sections", true);
        }
        if !matches!(ct, "," | ")" | "]" | ";" | "." | "?." | "(") {
            return rsharp_space(options, "after_attributes", true);
        }
    }
    if ct == "[" {
        if pt == "new" {
            return false;
        }
        if pt == "]" && prev.ast.bracket == BracketKind::Attribute {
            return true;
        }
        return match cur.ast.bracket {
            BracketKind::ElementAccess => {
                rsharp_space(options, "before_array_access_brackets", false)
            }
            BracketKind::ArrayRank => rsharp_space(options, "before_array_rank_brackets", false),
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
            OperatorKind::Ternary => rsharp_space(
                options,
                "before_ternary_quest",
                options.space.before_question_in_conditional,
            ),
            OperatorKind::Nullable | OperatorKind::None => {
                rsharp_space(options, "before_nullable_mark", false)
            }
            _ => is_word(prev),
        };
    }
    if pt == "?" {
        // `x as Key? ?? fallback` keeps the coalescing operator separated.
        if cur.text(src) == "??" {
            return true;
        }
        return match prev.ast.operator {
            OperatorKind::Ternary => rsharp_space(
                options,
                "after_ternary_quest",
                options.space.after_question_in_conditional,
            ),
            OperatorKind::Nullable | OperatorKind::None => is_word(cur),
            _ => is_word(cur),
        };
    }

    if cur.ast.operator == OperatorKind::Pointer {
        return rsharp_space(options, "before_pointer_asterik_declaration", false);
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
        return unary_after_space(options, pt);
    }
    if is_postfix_unary(src, tokens, i) {
        return rsharp_space(options, "near_postfix_and_prefix_op", false);
    }
    if is_postfix_unary(src, tokens, i - 1) {
        // `i++ == x`: the postfix operator still needs a space before a binary operator.
        return is_binary_op(src, tokens, i);
    }

    if pt == "operator" {
        return rsharp_space(options, "after_operator_keyword", true);
    }

    if is_binary_op(src, tokens, i) || is_binary_op(src, tokens, i - 1) {
        let operator = if is_binary_op(src, tokens, i) { ct } else { pt };
        if let Some(space) = binary_operator_space(options, operator) {
            return space;
        }
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
    layout: &Layout,
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
    if pt == "nameof" {
        return rsharp_space(options, "before_nameof_parentheses", false);
    }
    if pt == "new" {
        // `new()` in a type-parameter constraint is never spaced.
        if cur.ast.paren == ParenKind::ConstructorConstraint {
            return false;
        }
        // `new (T a, T b)[…]` (tuple type) needs the space; target-typed `new()` has its
        // own preference; `new Foo(…)` and `new string(…)` do not.
        if cur.ast.paren == ParenKind::ArgumentList {
            return rsharp_space(options, "before_new_parentheses", false);
        }
        return true;
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
        // `return (a, b)`, `throw (new T())`, `is (1, 2)`, `ref (int, int) x`: the keyword
        // and its operand/type follow the two `between_keyword_and_*` preferences.
        if matches!(
            pt,
            "return" | "throw" | "is" | "case" | "await" | "out" | "in" | "ref" | "yield"
        ) {
            return keyword_paren_space(src, tokens, layout, options, i);
        }
        if matches!(
            pt,
            "typeof" | "sizeof" | "default" | "this" | "base" | "new"
        ) {
            return false;
        }
        if matches!(
            cur.ast.paren,
            ParenKind::ParameterList
                | ParenKind::BracketedParameterList
                | ParenKind::PrimaryConstructorParameterList
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
            let empty = tokens.get(i + 1).is_some_and(|t| t.text(src) == ")");
            if empty {
                return rsharp_space(
                    options,
                    "before_empty_method_call_parentheses",
                    options.space.method_call_name_and_open_paren,
                );
            }
            before_paren_space(
                options,
                cur.ast.paren,
                options.space.method_call_name_and_open_paren,
            )
        }
        ParenKind::ParameterList
        | ParenKind::BracketedParameterList
        | ParenKind::PrimaryConstructorParameterList => {
            let empty = tokens.get(i + 1).is_some_and(|t| t.text(src) == ")");
            if empty {
                return rsharp_space(
                    options,
                    "before_empty_method_parentheses",
                    options.space.method_declaration_name_and_open_paren,
                );
            }
            before_paren_space(
                options,
                cur.ast.paren,
                options.space.method_declaration_name_and_open_paren,
            )
        }
        ParenKind::ConstructorInitializer | ParenKind::ConstructorConstraint => false,
        ParenKind::If
        | ParenKind::While
        | ParenKind::For
        | ParenKind::ForEach
        | ParenKind::Switch
        | ParenKind::Catch
        | ParenKind::Using
        | ParenKind::Lock
        | ParenKind::Fixed
        | ParenKind::Checked => before_paren_space(
            options,
            cur.ast.paren,
            options.space.after_keywords_in_control_flow,
        ),
        ParenKind::TypeOf | ParenKind::SizeOf | ParenKind::Default => {
            before_paren_space(options, cur.ast.paren, false)
        }
        ParenKind::Cast | ParenKind::Parenthesized | ParenKind::Tuple => {
            if prev.kind == TokenKind::Keyword {
                return keyword_paren_space(src, tokens, layout, options, i);
            }
            // Deliberately ignore `is_word(prev)`: a `(` right after an identifier is a
            // call/declaration that tree-sitter sometimes mislabels as a cast, and the
            // difference would make a second formatting pass change the output.
            binary_before || matches!(pt, "?" | ":" | "??" | "=>")
        }
        ParenKind::None | ParenKind::Other => {
            if prev.kind == TokenKind::Keyword {
                return keyword_paren_space(src, tokens, layout, options, i);
            }
            binary_before || is_word(prev) || matches!(pt, "?" | ":" | "??" | "=>")
        }
    }
}

/// Whether the `(` or `)` at `index` delimit a cast. Tree-sitter mislabels casts as
/// parenthesised expressions or parameter lists depending on the surrounding code, so a
/// parenthesised type in expression position is treated as one.
fn paren_is_cast_like(src: &str, tokens: &[Token], layout: &Layout, index: usize) -> bool {
    let open = if tokens[index].text(src) == ")" {
        layout.enclosing_open[index]
    } else {
        index
    };
    if open == usize::MAX || open == 0 {
        return false;
    }
    if tokens[open].ast.paren == ParenKind::Cast {
        return true;
    }
    if !matches!(
        tokens[open].ast.paren,
        ParenKind::None | ParenKind::Other | ParenKind::ParameterList | ParenKind::ArgumentList
    ) {
        return false;
    }
    matches!(
        tokens[open - 1].text(src),
        "=" | "("
            | "["
            | ","
            | "return"
            | "throw"
            | "=>"
            | "??"
            | "?"
            | ":"
            | "&&"
            | "||"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "!"
            | "is"
            | "case"
            | "out"
            | "ref"
            | "in"
    )
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
        ParenKind::ParameterList | ParenKind::PrimaryConstructorParameterList => {
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
        ColonKind::Ternary => rsharp_space(
            options,
            "before_ternary_colon",
            options.space.before_colon_in_conditional,
        ),
        ColonKind::SwitchLabel => rsharp_space(options, "before_colon_in_case", false),
        ColonKind::AttributeTarget => rsharp_space(options, "before_attribute_colon", false),
        ColonKind::Constraint => {
            rsharp_space(options, "before_type_parameter_constraint_colon", true)
        }
        ColonKind::ConstructorInitializer => true,
        ColonKind::NamedArgument | ColonKind::Label | ColonKind::Pattern => false,
        ColonKind::None | ColonKind::Other => false,
    }
}

fn colon_after(token: &Token, options: &FormatOptions) -> bool {
    match token.ast.colon {
        ColonKind::BaseList => options.space.after_colon_in_inheritance,
        ColonKind::Ternary => rsharp_space(
            options,
            "after_ternary_colon",
            options.space.after_colon_in_conditional,
        ),
        ColonKind::SwitchLabel => rsharp_space(options, "after_colon_in_case", true),
        ColonKind::AttributeTarget => rsharp_space(options, "after_attribute_colon", true),
        ColonKind::Constraint => {
            rsharp_space(options, "after_type_parameter_constraint_colon", true)
        }
        ColonKind::NamedArgument | ColonKind::Pattern | ColonKind::ConstructorInitializer => true,
        ColonKind::Label => true,
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
