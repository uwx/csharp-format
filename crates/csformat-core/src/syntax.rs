//! Tree-sitter based structural annotation.
//!
//! The formatter is token driven: it never reprints syntax, it only decides where whitespace
//! goes. Tree-sitter is used to answer the questions that cannot be answered locally, such as
//! "is this `<` a generic argument list or a less-than operator?", "is this `{` an initializer
//! or a method body?", or "is this `:` a switch label or a base-list colon?".
//!
//! Parse errors are tolerated. Tokens that fall inside `ERROR`/`MISSING` regions keep their
//! default annotation and the formatter falls back to local heuristics.

use crate::lexer::{Token, TokenKind};
use tree_sitter::{Node, Parser, Tree, TreeCursor};

/// Where ReSharper places a declaration's attribute sections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AttributePlacement {
    /// The token is not part of a declaration attribute.
    #[default]
    None,
    /// Attributes stay on the declaration line (fields, events).
    SameLine,
    /// Attributes stay on the declaration line when the declaration fits on one line,
    /// and move to their own line when it spans several lines (properties, indexers).
    SameLineIfSingleLine,
    /// Attributes get their own line (types, methods, local functions, ...).
    OwnLine,
}

/// Kind of the nearest enclosing declaration. Drives the blank-line rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum DeclKind {
    #[default]
    None,
    Namespace,
    Type,
    Field,
    Property,
    AutoProperty,
    Accessor,
    Invocable,
    LocalMethod,
}

/// Kind of a statement. Drives the blank-line rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum StmtKind {
    #[default]
    None,
    Block,
    Switch,
    If,
    Do,
    While,
    For,
    Foreach,
    Using,
    Lock,
    Try,
    Checked,
    UnsafeFixed,
    Return,
    Break,
    Continue,
    Throw,
    Goto,
    Yield,
    Expression,
    LocalDeclaration,
    Labeled,
    Other,
}

impl StmtKind {
    /// Statements that own a single unbraced child statement.
    #[must_use]
    pub fn is_single_statement_owner(self) -> bool {
        matches!(
            self,
            Self::If
                | Self::Do
                | Self::While
                | Self::For
                | Self::Foreach
                | Self::Using
                | Self::Lock
                | Self::Checked
                | Self::UnsafeFixed
        )
    }

    /// Statements that directly carry a control transfer.
    #[must_use]
    pub fn is_control_transfer(self) -> bool {
        matches!(
            self,
            Self::Return | Self::Break | Self::Continue | Self::Throw | Self::Goto | Self::Yield
        )
    }

    /// Kind of a "block statement" for the blank-line rules (see `FindBlock`).
    #[must_use]
    pub fn is_block_statement(self) -> bool {
        !matches!(
            self,
            Self::None
                | Self::Expression
                | Self::LocalDeclaration
                | Self::Return
                | Self::Break
                | Self::Continue
                | Self::Throw
                | Self::Goto
                | Self::Yield
                | Self::Other
        )
    }
}

/// Background facts about a token gathered from its position in the syntax tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AstInfo {
    pub flags: u32,
    pub brace: BraceKind,
    pub brace_owner: BraceOwner,
    pub paren: ParenKind,
    pub bracket: BracketKind,
    pub angle: AngleKind,
    pub colon: ColonKind,
    pub comma: CommaKind,
    pub semicolon: SemicolonKind,
    pub operator: OperatorKind,
    /// The token is the first token of a member/local-function declaration.
    pub member_start: bool,
    /// The token is the declared name of a type declaration (`class Foo`, `delegate void F`).
    pub type_name: bool,
    /// The token is an attribute bracket belonging to a declaration (not to a parameter).
    pub member_attribute: bool,
    /// The token is inside a `switch_section` (a `case` block).
    pub switch_section: bool,
    /// The enclosing switch section fits on a single source line.
    pub switch_section_inline: bool,
    /// The first statement of the enclosing switch section owns a block
    /// (`BLANK_LINES_AROUND_BLOCK_CASE_SECTION` before the section).
    pub switch_section_block_first: bool,
    /// The last statement of the enclosing switch section owns a block
    /// (`BLANK_LINES_AROUND_BLOCK_CASE_SECTION` after the section).
    pub switch_section_block_last: bool,
    /// The nearest enclosing declaration is a local function.
    pub local_function: bool,
    /// The nearest enclosing declaration is a type, delegate or namespace declaration.
    pub type_declaration: bool,
    /// The token is inside a `do` statement.
    pub do_statement: bool,
    /// The token is inside an `ERROR` node or is `MISSING`.
    pub in_error: bool,
    /// A tree-sitter node was found for this token.
    pub matched: bool,
    /// Where ReSharper places the enclosed declaration's attribute sections.
    pub attribute: AttributePlacement,
    /// Kind of the nearest enclosing declaration.
    pub decl: DeclKind,
    /// Kind of the outer declaration starting at this token.
    pub decl_start: DeclKind,
    /// Whether that declaration spans several source lines.
    pub decl_multiline: bool,
    /// Kind of the outer declaration ending at this token.
    pub decl_end: DeclKind,
    /// Whether that declaration spans several source lines.
    pub decl_end_multiline: bool,
    /// Kind of the outer statement starting at this token.
    pub stmt_start: StmtKind,
    /// When the token starts a statement that is the direct body of another statement,
    /// the kind of that enclosing statement.
    pub stmt_parent: StmtKind,
    /// Start byte of that enclosing statement.
    pub stmt_parent_start: usize,
    /// Whether the token belongs to a LINQ query expression.
    pub in_query: bool,
    /// Start byte of the innermost statement that contains the token.
    pub stmt_owner_start: usize,
    /// Whether that statement spans several source lines.
    pub stmt_start_multiline: bool,
    /// Whether that statement (or a nested unbraced body) owns a block.
    pub stmt_start_has_block: bool,
    /// Kind of the outer statement ending at this token.
    pub stmt_end: StmtKind,
    /// Whether that statement spans several source lines.
    pub stmt_end_multiline: bool,
    /// Whether that statement (or a nested unbraced body) owns a block.
    pub stmt_end_has_block: bool,
    /// The token terminates the last `using` directive of a using list.
    pub using_end: bool,
    /// Whether the token belongs to a `using` directive at namespace or file scope.
    pub using_directive: bool,
    /// The token starts a top-level `using` directive.
    pub using_start: bool,
    /// The token belongs to a `using static ...` directive.
    pub using_static: bool,
    /// `FindControlStatement` result for the statement starting at this token, as the bit
    /// flags of ReSharper's `ControlStatementKind`.
    pub stmt_start_control: u8,
    /// Nesting level of that control statement (`0` when the statement itself is one).
    pub stmt_start_control_level: u8,
    /// `FindControlStatement` result for the statement ending at this token.
    pub stmt_end_control: u8,
    /// Nesting level of that control statement.
    pub stmt_end_control_level: u8,
}

impl AstInfo {
    #[must_use]
    pub fn has(self, flag: u32) -> bool {
        self.flags & flag != 0
    }
}

pub mod flags {
    pub const ARGUMENT_LIST: u32 = 1 << 0;
    pub const PARAMETER_LIST: u32 = 1 << 1;
    pub const TYPE_ARGUMENT_LIST: u32 = 1 << 2;
    pub const TYPE_PARAMETER_LIST: u32 = 1 << 3;
    pub const ATTRIBUTE: u32 = 1 << 4;
    pub const BASE_LIST: u32 = 1 << 5;
    pub const CONSTRAINT: u32 = 1 << 6;
    pub const INITIALIZER: u32 = 1 << 7;
    pub const ACCESSOR_LIST: u32 = 1 << 8;
    pub const SWITCH_EXPRESSION: u32 = 1 << 9;
    pub const SWITCH_STATEMENT: u32 = 1 << 10;
    pub const FOR_STATEMENT: u32 = 1 << 11;
    pub const QUERY: u32 = 1 << 12;
    pub const LAMBDA: u32 = 1 << 13;
    pub const CAST: u32 = 1 << 14;
    pub const TUPLE: u32 = 1 << 15;
    pub const COLLECTION_EXPRESSION: u32 = 1 << 16;
    pub const ARRAY_RANK: u32 = 1 << 17;
    pub const USING_DIRECTIVE: u32 = 1 << 18;
    pub const PATTERN: u32 = 1 << 19;
    pub const NAMESPACE: u32 = 1 << 20;
    pub const TYPE_DECLARATION: u32 = 1 << 21;
    pub const ENUM: u32 = 1 << 22;
    pub const EXPRESSION_BODIED: u32 = 1 << 23;
    pub const ANONYMOUS_TYPE: u32 = 1 << 24;
    pub const ANONYMOUS_METHOD: u32 = 1 << 25;
    pub const LOCAL_FUNCTION: u32 = 1 << 26;
    pub const METHOD_DECLARATION: u32 = 1 << 27;
    pub const PROPERTY: u32 = 1 << 28;
    pub const CONDITIONAL_ACCESS: u32 = 1 << 29;
    pub const INTERPOLATION: u32 = 1 << 30;
    pub const CATCH_FILTER: u32 = 1 << 31;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum BraceKind {
    #[default]
    None,
    Block,
    TypeBody,
    NamespaceBody,
    EnumBody,
    Initializer,
    AccessorList,
    SwitchExpression,
    SwitchStatement,
    Pattern,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum BraceOwner {
    #[default]
    None,
    Method,
    LocalFunction,
    Constructor,
    Destructor,
    Operator,
    Accessor,
    Lambda,
    AnonymousMethod,
    Control,
    Type,
    Namespace,
    Enum,
    Property,
    AnonymousType,
    Initializer,
    SwitchExpression,
    Pattern,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ParenKind {
    #[default]
    None,
    ArgumentList,
    ParameterList,
    /// The parameter list of a primary constructor (`class C(int a)`).
    PrimaryConstructorParameterList,
    BracketedParameterList,
    Parenthesized,
    Tuple,
    Cast,
    If,
    While,
    For,
    ForEach,
    Switch,
    Catch,
    Using,
    Lock,
    Fixed,
    Checked,
    TypeOf,
    SizeOf,
    Default,
    AttributeArguments,
    ConstructorInitializer,
    /// The empty `()` of a `new()` type-parameter constraint.
    ConstructorConstraint,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum BracketKind {
    #[default]
    None,
    Attribute,
    ArrayRank,
    ElementAccess,
    CollectionExpression,
    ListPattern,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum AngleKind {
    #[default]
    None,
    GenericOpen,
    GenericClose,
    TypeParameterOpen,
    TypeParameterClose,
    Operator,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ColonKind {
    #[default]
    None,
    BaseList,
    Constraint,
    Ternary,
    NamedArgument,
    SwitchLabel,
    Label,
    Pattern,
    ConstructorInitializer,
    AttributeTarget,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum CommaKind {
    #[default]
    None,
    ArgumentList,
    ParameterList,
    Initializer,
    SwitchExpression,
    EnumBody,
    Tuple,
    CollectionExpression,
    TypeArguments,
    TypeParameters,
    AttributeArguments,
    /// Comma separating entries of a base list (`class C : A, B`).
    BaseList,
    /// Comma separating declarators of one declaration (`int a = 1, b = 2;`).
    VariableDeclaration,
    /// Comma inside a multidimensional array rank (`new int[,]`).
    ArrayRank,
    /// Comma between constraints of one `where` clause.
    Constraints,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum SemicolonKind {
    #[default]
    None,
    ForStatement,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum OperatorKind {
    #[default]
    None,
    Binary,
    UnaryPrefix,
    UnaryPostfix,
    Pointer,
    MemberAccess,
    Range,
    Nullable,
    Ternary,
}

/// A parsed document plus the ability to annotate its tokens.
pub struct Syntax {
    tree: Tree,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("has_errors", &self.has_errors())
            .field("error_nodes", &self.error_count())
            .finish()
    }
}

impl Syntax {
    /// Parse `source`, always returning a tree even for invalid input.
    #[must_use]
    pub fn parse(source: &str) -> Self {
        let mut parser = Parser::new();
        // The generated parser is statically linked, so this only fails on an ABI mismatch.
        let _ = parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into());
        let tree = parser.parse(source, None).unwrap_or_else(|| {
            let mut fallback = Parser::new();
            let _ = fallback.set_language(&tree_sitter_c_sharp::LANGUAGE.into());
            fallback
                .parse("", None)
                .expect("empty parse always succeeds")
        });
        Self { tree }
    }

    #[must_use]
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    #[must_use]
    pub fn root(&self) -> Node<'_> {
        self.tree.root_node()
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.tree.root_node().has_error()
    }

    /// Number of `ERROR`/`MISSING` nodes. Used by the verification harness.
    #[must_use]
    pub fn error_count(&self) -> usize {
        let mut count = 0usize;
        let mut stack = vec![self.tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "ERROR" || node.is_missing() {
                count += 1;
            }
            let mut child = node.walk();
            for c in node.children(&mut child) {
                stack.push(c);
            }
        }
        count
    }

    /// First line (1-based) containing a parse error, if any.
    #[must_use]
    pub fn first_error_line(&self) -> Option<usize> {
        let mut stack = vec![self.tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "ERROR" || node.is_missing() {
                return Some(node.start_position().row + 1);
            }
            let mut child = node.walk();
            for c in node.children(&mut child) {
                stack.push(c);
            }
        }
        None
    }

    /// Annotate every token with syntax context.
    pub fn annotate(&self, source: &str, tokens: &mut [Token]) {
        // Tokens are in source order, so one cursor can walk the tree monotonically instead
        // of restarting at the root for every token. Restarting is O(tree depth) per token,
        // which becomes quadratic on long member-access/invocation chains.
        let mut cursor = self.tree.walk();
        for token in tokens.iter_mut() {
            token.ast = self.annotate_token(&mut cursor, source, token);
        }
    }

    fn annotate_token(&self, cursor: &mut TreeCursor<'_>, source: &str, token: &Token) -> AstInfo {
        if token.kind == TokenKind::Directive {
            return AstInfo::default();
        }

        // Climb until the cursor node covers the token, then descend to the deepest node
        // that still does.
        loop {
            let node = cursor.node();
            if node.start_byte() <= token.start && node.end_byte() > token.start {
                break;
            }
            if !cursor.goto_parent() {
                break;
            }
        }
        while cursor.goto_first_child_for_byte(token.start).is_some() {}

        let mut info = AstInfo {
            matched: true,
            ..AstInfo::default()
        };

        // Walk the ancestors with the cursor. `Node::parent` re-descends from the tree root
        // (O(depth)), so using it in this loop would make annotation quadratic on deep trees;
        // cursor moves are O(1).
        let mut parent_kind = "";
        let mut grandparent_kind = "";
        let mut parent_start = token.start;
        let mut depth = 0;
        let mut nearest_decl = "";
        let mut nearest_decl_node = None;
        let mut decl_start = DeclKind::None;
        let mut decl_multiline = false;
        let mut decl_end = DeclKind::None;
        let mut decl_end_multiline = false;
        let mut stmt_start = StmtKind::None;
        let mut stmt_parent = StmtKind::None;
        let mut stmt_parent_start = 0usize;
        let mut in_query = false;
        let mut stmt_owner_start = usize::MAX;
        let mut stmt_start_multiline = false;
        let mut stmt_start_has_block = false;
        let mut stmt_start_control = 0u8;
        let mut stmt_start_control_level = 0u8;
        let mut stmt_end = StmtKind::None;
        let mut stmt_end_multiline = false;
        let mut stmt_end_has_block = false;
        let mut stmt_end_control = 0u8;
        let mut stmt_end_control_level = 0u8;
        loop {
            let node = cursor.node();
            let kind = node.kind();
            if depth == 1 {
                parent_kind = kind;
                parent_start = node.start_byte();
            } else if depth == 2 {
                grandparent_kind = kind;
            }
            if kind == "ERROR" || node.is_missing() {
                info.in_error = true;
            }
            if let Some(flag) = flag_for_kind(kind) {
                info.flags |= flag;
            }
            if is_statement_kind(kind) && stmt_owner_start == usize::MAX {
                stmt_owner_start = node.start_byte();
            }
            if kind == "query_expression" {
                in_query = true;
            }
            if kind == "arrow_expression_clause" {
                info.flags |= flags::EXPRESSION_BODIED;
            }
            if nearest_decl.is_empty() && is_member_declaration(kind) {
                nearest_decl = kind;
                nearest_decl_node = Some(node);
            }
            if is_unit_declaration(kind) {
                let multiline = source
                    .get(node.byte_range())
                    .is_some_and(|text| text.contains('\n'));
                if node.start_byte() == token.start {
                    decl_start = decl_kind_of(kind, node);
                    decl_multiline = multiline;
                }
                if node.end_byte() == token.end {
                    decl_end = decl_kind_of(kind, node);
                    decl_end_multiline = multiline;
                }
            }
            if is_statement_kind(kind) {
                let multiline = source
                    .get(node.byte_range())
                    .is_some_and(|text| text.contains('\n'));
                if node.start_byte() == token.start {
                    stmt_start = stmt_kind(kind);
                    if let Some(parent) = node.parent() {
                        if stmt_kind(parent.kind()).is_single_statement_owner() {
                            stmt_parent = stmt_kind(parent.kind());
                            stmt_parent_start = parent.start_byte();
                        }
                    }
                    stmt_start_multiline = multiline;
                    stmt_start_has_block = find_block(node);
                    let (control, level) = find_control_statement(node);
                    stmt_start_control = control;
                    stmt_start_control_level = level;
                }
                if node.end_byte() == token.end {
                    stmt_end = stmt_kind(kind);
                    stmt_end_multiline = multiline;
                    stmt_end_has_block = find_block(node);
                    let (control, level) = find_control_statement(node);
                    stmt_end_control = control;
                    stmt_end_control_level = level;
                }
            }
            if kind == "switch_section" && !info.switch_section {
                info.switch_section = true;
                info.switch_section_inline = source
                    .get(node.byte_range())
                    .is_none_or(|text| !text.contains('\n'));
                let mut section_cursor = node.walk();
                let mut first_block = None;
                let mut last_block = None;
                for child in node.named_children(&mut section_cursor) {
                    if !is_statement_kind(child.kind()) {
                        continue;
                    }
                    if first_block.is_none() {
                        first_block = Some(find_block(child));
                    }
                    last_block = Some(find_block(child));
                }
                info.switch_section_block_first = first_block.unwrap_or(false);
                info.switch_section_block_last = last_block.unwrap_or(false);
            }
            if kind == "do_statement" && depth == 1 {
                // Only the terminating `;` of `do { } while (...);` is a direct child of the
                // do statement; the flag must not leak to statements inside its body.
                info.do_statement = true;
            }
            if is_member_declaration(kind) && node.start_byte() == token.start {
                info.member_start = true;
            }
            if depth == 1 && kind == "using_directive" {
                info.using_directive = true;
                if node.start_byte() == token.start {
                    info.using_start = true;
                    info.using_static = source
                        .get(node.byte_range())
                        .is_some_and(|text| text.trim_start().starts_with("using static"));
                } else {
                    info.using_static = source
                        .get(node.byte_range())
                        .is_some_and(|text| text.trim_start().starts_with("using static"));
                }
                if node.end_byte() == token.end {
                    let mut sibling = node.next_named_sibling();
                    while let Some(sib) = sibling {
                        if sib.kind() == "comment" {
                            sibling = sib.next_named_sibling();
                            continue;
                        }
                        break;
                    }
                    info.using_end = sibling.is_none_or(|next| next.kind() != "using_directive");
                }
            }
            if depth == 1 && is_named_type_declaration(kind) {
                info.type_name = node
                    .child_by_field_name("name")
                    .is_some_and(|name| name.start_byte() == token.start);
            }
            depth += 1;
            if depth > 64 || !cursor.goto_parent() {
                break;
            }
        }

        // Restore the cursor to the leaf so the next token can walk from here.
        while cursor.goto_first_child_for_byte(token.start).is_some() {}
        let leaf = cursor.node();

        info.brace = brace_kind(parent_kind, grandparent_kind);
        info.brace_owner = brace_owner(parent_kind, grandparent_kind);
        info.paren = paren_kind(parent_kind, grandparent_kind);
        info.bracket = bracket_kind(parent_kind);
        info.member_attribute =
            info.bracket == BracketKind::Attribute && is_member_declaration(grandparent_kind);
        info.colon = colon_kind(parent_kind);
        info.comma = comma_kind(parent_kind);
        info.semicolon = semicolon_kind(parent_kind);
        info.angle = angle_kind(parent_kind, token.start == parent_start);
        info.operator = operator_kind(token, parent_kind);
        info.decl =
            nearest_decl_node.map_or(DeclKind::None, |node| decl_kind_of(nearest_decl, node));
        info.decl_start = decl_start;
        info.decl_multiline = decl_multiline;
        info.decl_end = decl_end;
        info.decl_end_multiline = decl_end_multiline;
        info.stmt_start = stmt_start;
        info.stmt_parent = stmt_parent;
        info.stmt_parent_start = stmt_parent_start;
        info.in_query = in_query;
        info.stmt_owner_start = stmt_owner_start;
        info.stmt_start_multiline = stmt_start_multiline;
        info.stmt_start_has_block = stmt_start_has_block;
        info.stmt_end = stmt_end;
        info.stmt_end_multiline = stmt_end_multiline;
        info.stmt_end_has_block = stmt_end_has_block;
        info.stmt_start_control = stmt_start_control;
        info.stmt_start_control_level = stmt_start_control_level;
        info.stmt_end_control = stmt_end_control;
        info.stmt_end_control_level = stmt_end_control_level;
        info.local_function = nearest_decl == "local_function_statement";
        info.type_declaration = is_type_declaration(nearest_decl);
        info.attribute = if nearest_decl.is_empty() {
            AttributePlacement::None
        } else if matches!(
            nearest_decl,
            "field_declaration" | "event_field_declaration"
        ) {
            AttributePlacement::SameLine
        } else if matches!(
            nearest_decl,
            "property_declaration" | "indexer_declaration" | "event_declaration"
        ) {
            // ReSharper keeps these on the declaration line only while the declaration
            // (accessors, expression body or initializer) fits on a single line.
            AttributePlacement::SameLineIfSingleLine
        } else {
            AttributePlacement::OwnLine
        };

        // `G(a * b)` is ambiguous: tree-sitter reads it as a C# 7 declaration expression
        // with a pointer type (`G(a* b)`). A real declaration expression needs `out`
        // (or `ref`), so without one this is ordinary multiplication.
        if info.operator == OperatorKind::Pointer && !pointer_is_declaration(leaf) {
            info.operator = OperatorKind::Binary;
        }
        info
    }
}

fn brace_kind(parent: &str, grandparent: &str) -> BraceKind {
    match parent {
        "block" => BraceKind::Block,
        "declaration_list"
            if matches!(
                grandparent,
                "namespace_declaration" | "file_scoped_namespace_declaration" | "namespace_body"
            ) =>
        {
            BraceKind::NamespaceBody
        }
        "declaration_list" => BraceKind::TypeBody,
        "enum_member_declaration_list" => BraceKind::EnumBody,
        "initializer_expression"
        | "with_initializer"
        | "with_expression"
        | "anonymous_object_creation_expression" => BraceKind::Initializer,
        "accessor_list" => BraceKind::AccessorList,
        "switch_expression" => BraceKind::SwitchExpression,
        "switch_body" => BraceKind::SwitchStatement,
        "property_pattern_clause" | "recursive_pattern" | "list_pattern" => BraceKind::Pattern,
        "" => BraceKind::None,
        _ => BraceKind::Other,
    }
}

fn brace_owner(parent: &str, grandparent: &str) -> BraceOwner {
    match parent {
        "block" => match grandparent {
            "method_declaration" => BraceOwner::Method,
            "local_function_statement" => BraceOwner::LocalFunction,
            "constructor_declaration" => BraceOwner::Constructor,
            "destructor_declaration" => BraceOwner::Destructor,
            "operator_declaration" | "conversion_operator_declaration" => BraceOwner::Operator,
            "accessor_declaration" => BraceOwner::Accessor,
            "lambda_expression" => BraceOwner::Lambda,
            "anonymous_method_expression" => BraceOwner::AnonymousMethod,
            "if_statement"
            | "else_clause"
            | "while_statement"
            | "for_statement"
            | "foreach_statement"
            | "do_statement"
            | "using_statement"
            | "lock_statement"
            | "fixed_statement"
            | "try_statement"
            | "catch_clause"
            | "finally_clause"
            | "switch_section"
            | "checked_expression"
            | "unchecked_expression" => BraceOwner::Control,
            _ => BraceOwner::Other,
        },
        "declaration_list" => match grandparent {
            "namespace_declaration" => BraceOwner::Namespace,
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "delegate_declaration" => BraceOwner::Type,
            "enum_declaration" => BraceOwner::Enum,
            _ => BraceOwner::Type,
        },
        "enum_member_declaration_list" => BraceOwner::Enum,
        "initializer_expression" => BraceOwner::Initializer,
        "accessor_list" => BraceOwner::Property,
        "switch_expression" => BraceOwner::SwitchExpression,
        "switch_body" => BraceOwner::Control,
        "property_pattern_clause" | "recursive_pattern" => BraceOwner::Pattern,
        "with_initializer" => BraceOwner::Initializer,
        _ => BraceOwner::Other,
    }
}

fn paren_kind(parent: &str, grandparent: &str) -> ParenKind {
    match parent {
        "argument_list" => ParenKind::ArgumentList,
        "parameter_list" | "bracketed_parameter_list" => {
            // Lambda / anonymous-method parameter lists are just parenthesized lists.
            if matches!(
                grandparent,
                "lambda_expression" | "anonymous_method_expression"
            ) {
                ParenKind::Other
            } else if parent == "bracketed_parameter_list" {
                ParenKind::BracketedParameterList
            } else if matches!(
                grandparent,
                "class_declaration" | "struct_declaration" | "record_declaration"
            ) {
                ParenKind::PrimaryConstructorParameterList
            } else {
                ParenKind::ParameterList
            }
        }
        "parenthesized_expression" => ParenKind::Parenthesized,
        "tuple_expression" | "tuple_pattern" => ParenKind::Tuple,
        "cast_expression" => ParenKind::Cast,
        "if_statement" => ParenKind::If,
        "while_statement" => ParenKind::While,
        "for_statement" => ParenKind::For,
        "foreach_statement" => ParenKind::ForEach,
        "switch_statement" => ParenKind::Switch,
        "catch_clause" | "catch_declaration" => ParenKind::Catch,
        "do_statement" => ParenKind::While,
        "using_statement" => ParenKind::Using,
        "lock_statement" => ParenKind::Lock,
        "fixed_statement" | "fixed_pointer_declaration" => ParenKind::Fixed,
        "checked_expression" | "unchecked_expression" => ParenKind::Checked,
        "typeof_expression" => ParenKind::TypeOf,
        "sizeof_expression" => ParenKind::SizeOf,
        "default_expression" => ParenKind::Default,
        "attribute_argument_list" => ParenKind::AttributeArguments,
        "constructor_initializer" => ParenKind::ConstructorInitializer,
        "constructor_constraint" => ParenKind::ConstructorConstraint,
        "" => ParenKind::None,
        _ => ParenKind::Other,
    }
}

fn bracket_kind(parent: &str) -> BracketKind {
    match parent {
        "attribute_list" | "global_attribute" | "attribute" => BracketKind::Attribute,
        "array_rank_specifier" | "array_type" => BracketKind::ArrayRank,
        "element_access_expression" | "bracketed_argument_list" | "bracketed_parameter_list" => {
            BracketKind::ElementAccess
        }
        "collection_expression" => BracketKind::CollectionExpression,
        "list_pattern" => BracketKind::ListPattern,
        "" => BracketKind::None,
        _ => BracketKind::Other,
    }
}

fn colon_kind(parent: &str) -> ColonKind {
    match parent {
        "base_list" => ColonKind::BaseList,
        "type_parameter_constraints_clause" => ColonKind::Constraint,
        "conditional_expression" => ColonKind::Ternary,
        "argument" | "attribute_argument" => ColonKind::NamedArgument,
        "switch_section" => ColonKind::SwitchLabel,
        "labeled_statement" => ColonKind::Label,
        "subpattern" => ColonKind::Pattern,
        "constructor_initializer" => ColonKind::ConstructorInitializer,
        "global_attribute" => ColonKind::AttributeTarget,
        "" => ColonKind::None,
        _ => ColonKind::Other,
    }
}

fn comma_kind(parent: &str) -> CommaKind {
    match parent {
        "argument_list" => CommaKind::ArgumentList,
        "parameter_list" | "bracketed_parameter_list" => CommaKind::ParameterList,
        "base_list" => CommaKind::BaseList,
        "variable_declaration" | "using_variable_declaration" => CommaKind::VariableDeclaration,
        "array_rank_specifier" => CommaKind::ArrayRank,
        "initializer_expression" | "with_initializer" | "with_expression" => CommaKind::Initializer,
        "switch_expression" => CommaKind::SwitchExpression,
        "enum_member_declaration_list" => CommaKind::EnumBody,
        "tuple_expression" => CommaKind::Tuple,
        "collection_expression" => CommaKind::CollectionExpression,
        "type_argument_list" => CommaKind::TypeArguments,
        "type_parameter_constraints_clause" => CommaKind::Constraints,
        "type_parameter_list" => CommaKind::TypeParameters,
        "attribute_argument_list" => CommaKind::AttributeArguments,
        "" => CommaKind::None,
        _ => CommaKind::Other,
    }
}

fn semicolon_kind(parent: &str) -> SemicolonKind {
    match parent {
        "for_statement" => SemicolonKind::ForStatement,
        "" => SemicolonKind::None,
        _ => SemicolonKind::Other,
    }
}

fn angle_kind(parent_kind: &str, at_start: bool) -> AngleKind {
    // Which side of a generic list does this leaf sit on? A `<` is the first child; a `>`
    // (or the first `>` of a `>>` token) is not at the list's start.
    let is_open = parent_kind == "type_argument_list" || parent_kind == "type_parameter_list";
    if !is_open {
        return AngleKind::None;
    }
    match parent_kind {
        "type_argument_list" => {
            if at_start {
                AngleKind::GenericOpen
            } else {
                AngleKind::GenericClose
            }
        }
        "type_parameter_list" => {
            if at_start {
                AngleKind::TypeParameterOpen
            } else {
                AngleKind::TypeParameterClose
            }
        }
        _ => AngleKind::None,
    }
}

/// Whether a `*` leaf inside a `pointer_type` really declares a pointer.
///
/// Parameters, casts and field/variable types do. A `declaration_expression` only does when
/// it carries an `out`/`ref` modifier; otherwise tree-sitter misread a multiplication.
fn pointer_is_declaration(leaf: Node<'_>) -> bool {
    let Some(pointer) = leaf.parent() else {
        return true;
    };
    if pointer.kind() != "pointer_type" && pointer.kind() != "function_pointer_type" {
        return true;
    }
    let Some(owner) = pointer.parent() else {
        return true;
    };
    if owner.kind() != "declaration_expression" {
        return true;
    }
    let mut cursor = owner.walk();
    let has_modifier = owner
        .children(&mut cursor)
        .any(|child| matches!(child.kind(), "out" | "ref" | "in"));
    has_modifier
}

fn operator_kind(token: &Token, parent: &str) -> OperatorKind {
    // Expression nodes span their operands too; only the punctuation token itself is the
    // operator. Without this, `ref _lexer!` tagged `_lexer` as a postfix operator and the
    // emitter dropped the space after `ref`.
    if token.kind != TokenKind::Punct {
        return OperatorKind::None;
    }
    match parent {
        "pointer_type" | "function_pointer_type" => OperatorKind::Pointer,
        "binary_expression" | "assignment_expression" | "is_pattern_expression" => {
            OperatorKind::Binary
        }
        "prefix_unary_expression" => OperatorKind::UnaryPrefix,
        "postfix_unary_expression" => OperatorKind::UnaryPostfix,
        "member_access_expression" | "conditional_access_expression" => OperatorKind::MemberAccess,
        "nullable_type" => OperatorKind::Nullable,
        "conditional_expression" => OperatorKind::Ternary,
        "" => OperatorKind::None,
        _ => OperatorKind::None,
    }
}

/// Declarations that start a blank-line "unit": members plus accessors and enum members.
fn is_unit_declaration(kind: &str) -> bool {
    is_member_declaration(kind)
        || matches!(kind, "accessor_declaration" | "enum_member_declaration")
}

/// Map a declaration node kind to the blank-line declaration kind.
fn decl_kind_of(kind: &str, node: Node) -> DeclKind {
    match kind {
        "namespace_declaration" | "file_scoped_namespace_declaration" => DeclKind::Namespace,
        "class_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "record_struct_declaration"
        | "delegate_declaration"
        | "enum_declaration" => DeclKind::Type,
        "field_declaration" | "event_field_declaration" | "enum_member_declaration" => {
            DeclKind::Field
        }
        "property_declaration" | "indexer_declaration" | "event_declaration" => {
            if is_auto_property(node) {
                DeclKind::AutoProperty
            } else {
                DeclKind::Property
            }
        }
        "accessor_declaration" => DeclKind::Accessor,
        "method_declaration"
        | "constructor_declaration"
        | "destructor_declaration"
        | "operator_declaration"
        | "conversion_operator_declaration" => DeclKind::Invocable,
        "local_function_statement" => DeclKind::LocalMethod,
        _ => DeclKind::None,
    }
}

/// A property whose accessors are all body-less (`{ get; set; }`).
fn is_auto_property(node: Node) -> bool {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        if child.kind() != "accessor_list" {
            continue;
        }
        let mut inner = child.walk();
        let accessors: Vec<Node> = child.named_children(&mut inner).collect();
        for accessor in accessors {
            let mut ac = accessor.walk();
            let parts: Vec<Node> = accessor.named_children(&mut ac).collect();
            if parts
                .iter()
                .any(|part| matches!(part.kind(), "block" | "arrow_expression_clause"))
            {
                return false;
            }
        }
        return true;
    }
    false
}

/// Statement node kinds.
fn is_statement_kind(kind: &str) -> bool {
    stmt_kind(kind) != StmtKind::None
}

/// Statements that own a single unbraced child statement (`SingleStatementOwnerStatements`).
fn is_single_statement_owner(kind: &str) -> bool {
    matches!(
        kind,
        "if_statement"
            | "do_statement"
            | "while_statement"
            | "for_statement"
            | "foreach_statement"
            | "using_statement"
            | "lock_statement"
            | "fixed_statement"
    )
}

/// ReSharper's `StatementSpacingUtil.GetControlStatementKind`.
fn control_kind(node: Node) -> u8 {
    match node.kind() {
        "return_statement" => control::RETURN,
        "continue_statement" => control::CONTINUE,
        "break_statement" => control::BREAK,
        "throw_statement" => control::THROW,
        "goto_statement" => control::GOTO,
        "yield_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "return" => return control::YIELD_RETURN,
                    "break" => return control::YIELD_BREAK,
                    _ => {}
                }
            }
            0
        }
        _ => 0,
    }
}

fn stmt_kind(kind: &str) -> StmtKind {
    match kind {
        "block" => StmtKind::Block,
        "switch_statement" => StmtKind::Switch,
        "if_statement" => StmtKind::If,
        "do_statement" => StmtKind::Do,
        "while_statement" => StmtKind::While,
        "for_statement" => StmtKind::For,
        "foreach_statement" => StmtKind::Foreach,
        "using_statement" => StmtKind::Using,
        "lock_statement" => StmtKind::Lock,
        "try_statement" => StmtKind::Try,
        "checked_statement" | "unchecked_statement" => StmtKind::Checked,
        "unsafe_statement" | "fixed_statement" => StmtKind::UnsafeFixed,
        "return_statement" => StmtKind::Return,
        "break_statement" => StmtKind::Break,
        "continue_statement" => StmtKind::Continue,
        "throw_statement" => StmtKind::Throw,
        "goto_statement" => StmtKind::Goto,
        "yield_statement" => StmtKind::Yield,
        "expression_statement" => StmtKind::Expression,
        "local_declaration_statement" => StmtKind::LocalDeclaration,
        "local_function_statement" | "empty_statement" => StmtKind::Other,
        "labeled_statement" => StmtKind::Labeled,
        _ => StmtKind::None,
    }
}

/// ReSharper's `FindBlock`: whether the statement owns (or nests an unbraced statement that
/// owns) a block, switch or try statement.
fn find_block(node: Node) -> bool {
    if matches!(
        node.kind(),
        "block"
            | "switch_statement"
            | "try_statement"
            | "checked_statement"
            | "unchecked_statement"
            | "unsafe_statement"
    ) {
        return true;
    }
    if !matches!(
        node.kind(),
        "if_statement"
            | "do_statement"
            | "while_statement"
            | "for_statement"
            | "foreach_statement"
            | "using_statement"
            | "lock_statement"
            | "fixed_statement"
            | "else_clause"
    ) {
        return false;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    children.into_iter().any(find_block)
}

/// Bit flags of ReSharper's `StatementSpacingUtil.ControlStatementKind`.
pub mod control {
    pub const RETURN: u8 = 1;
    pub const BREAK: u8 = 2;
    pub const CONTINUE: u8 = 4;
    pub const THROW: u8 = 8;
    pub const GOTO: u8 = 0x10;
    pub const YIELD_RETURN: u8 = 0x20;
    pub const YIELD_BREAK: u8 = 0x40;
}

/// ReSharper's `StatementSpacingUtil.FindControlStatement`: the control-transfer statements
/// reached through a chain of unbraced single-statement owners, plus the nesting depth of
/// that chain (`0` when the statement itself transfers control).
///
/// The result accumulates (bitwise OR) over every branch of an owner, and `else` clauses are
/// not visited at all: ReSharper only walks *statement* children of a statement node.
fn find_control_statement(node: Node) -> (u8, u8) {
    fn walk(node: Node, level: &mut u8) -> u8 {
        let kind = control_kind(node);
        if kind != 0 {
            return kind;
        }
        if !is_single_statement_owner(node.kind()) {
            return 0;
        }
        *level = level.saturating_add(1);
        let mut cursor = node.walk();
        let mut mask = 0u8;
        for child in node.children(&mut cursor) {
            if is_statement_kind(child.kind()) {
                mask |= walk(child, level);
            }
        }
        mask
    }

    let mut level = 0u8;
    let control = walk(node, &mut level);
    (control, level)
}

/// ReSharper's `StatementSpacingUtil.ShouldSeparate`: whether two sibling statements need a
/// blank line between them because one of them transfers control.
#[must_use]
pub fn should_separate(left: (u8, u8), right: (u8, u8)) -> bool {
    let (left_kind, left_level) = left;
    if left_kind == 0 {
        return false;
    }
    let (right_kind, right_level) = right;
    if right_kind == 0 {
        return true;
    }
    if left_level != right_level {
        return true;
    }
    if left_level > 1 {
        return true;
    }
    left_kind != right_kind
}

fn is_member_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "property_declaration"
            | "indexer_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration"
            | "field_declaration"
            | "event_declaration"
            | "event_field_declaration"
            | "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "enum_declaration"
            | "delegate_declaration"
            | "namespace_declaration"
            | "file_scoped_namespace_declaration"
            | "local_function_statement"
            | "global_statement"
    )
}

fn is_type_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "enum_declaration"
            | "delegate_declaration"
            | "namespace_declaration"
            | "file_scoped_namespace_declaration"
    )
}

/// Declarations whose name ReSharper may move onto a continuation line.
fn is_named_type_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "enum_declaration"
            | "delegate_declaration"
    )
}

fn flag_for_kind(kind: &str) -> Option<u32> {
    Some(match kind {
        "argument_list" => flags::ARGUMENT_LIST,
        "parameter_list" | "bracketed_parameter_list" => flags::PARAMETER_LIST,
        "type_argument_list" => flags::TYPE_ARGUMENT_LIST,
        "type_parameter_list" => flags::TYPE_PARAMETER_LIST,
        "attribute_list" | "attribute" | "global_attribute" | "attribute_argument_list" => {
            flags::ATTRIBUTE
        }
        "base_list" => flags::BASE_LIST,
        "type_parameter_constraints_clause" | "type_parameter_constraint" => flags::CONSTRAINT,
        "initializer_expression" | "with_initializer" => flags::INITIALIZER,
        "accessor_list" | "accessor_declaration" => flags::ACCESSOR_LIST,
        "switch_expression" => flags::SWITCH_EXPRESSION,
        "switch_statement" => flags::SWITCH_STATEMENT,
        "for_statement" => flags::FOR_STATEMENT,
        "query_expression" => flags::QUERY,
        "lambda_expression" | "anonymous_method_expression" => flags::LAMBDA,
        "cast_expression" => flags::CAST,
        "tuple_expression" => flags::TUPLE,
        "collection_expression" => flags::COLLECTION_EXPRESSION,
        "array_rank_specifier" | "array_type" => flags::ARRAY_RANK,
        "using_directive" => flags::USING_DIRECTIVE,
        "property_pattern_clause" | "recursive_pattern" | "subpattern" | "list_pattern" => {
            flags::PATTERN
        }
        "namespace_declaration" | "file_scoped_namespace_declaration" => flags::NAMESPACE,
        "class_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "delegate_declaration" => flags::TYPE_DECLARATION,
        "enum_declaration" => flags::ENUM,
        "arrow_expression_clause" => flags::EXPRESSION_BODIED,
        "anonymous_object_creation_expression" => flags::ANONYMOUS_TYPE,
        "local_function_statement" => flags::LOCAL_FUNCTION,
        "method_declaration" => flags::METHOD_DECLARATION,
        "property_declaration" | "indexer_declaration" => flags::PROPERTY,
        "conditional_access_expression" => flags::CONDITIONAL_ACCESS,
        "interpolation" => flags::INTERPOLATION,
        "catch_filter_clause" | "when_clause" => flags::CATCH_FILTER,
        _ => return None,
    })
}

/// Convenience: parse and annotate in one step.
#[must_use]
pub fn parse_and_annotate(source: &str) -> (Syntax, Vec<Token>) {
    let syntax = Syntax::parse(source);
    let mut tokens = crate::lexer::lex(source).tokens;
    syntax.annotate(source, &mut tokens);
    (syntax, tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn annotate(src: &str) -> Vec<(String, AstInfo)> {
        let syntax = Syntax::parse(src);
        let mut tokens = lex(src).tokens;
        syntax.annotate(src, &mut tokens);
        tokens
            .into_iter()
            .map(|t| (t.text(src).to_owned(), t.ast))
            .collect()
    }

    #[test]
    fn classifies_braces_and_parens() {
        let src = "class C { void M() { var x = new Foo(1) { A = 1 }; } }";
        let toks = annotate(src);
        let find = |text: &str, nth: usize| {
            toks.iter()
                .filter(|(t, _)| t == text)
                .nth(nth)
                .map(|(_, a)| *a)
                .unwrap()
        };
        assert_eq!(find("{", 0).brace, BraceKind::TypeBody);
        assert_eq!(find("{", 1).brace, BraceKind::Block);
        assert_eq!(find("{", 2).brace, BraceKind::Initializer);
        assert_eq!(find("(", 0).paren, ParenKind::ParameterList);
        assert_eq!(find("(", 1).paren, ParenKind::ArgumentList);
    }

    #[test]
    fn classifies_generics_and_operators() {
        let src = "List<List<int>> x; bool y = a < b;";
        let toks = annotate(src);
        let angles: Vec<_> = toks
            .iter()
            .filter(|(t, _)| t == "<" || t == ">" || t == ">>")
            .map(|(t, a)| (t.as_str(), a.angle))
            .collect();
        assert_eq!(angles[0], ("<", AngleKind::GenericOpen));
        assert_eq!(angles[1], ("<", AngleKind::GenericOpen));
        assert_eq!(angles[2], (">>", AngleKind::GenericClose));
        assert_eq!(angles[3], ("<", AngleKind::None));
    }

    #[test]
    fn classifies_colons() {
        let src = "class C : B { int P { get; } void M() { switch (x) { case 1: break; } } }";
        let toks = annotate(src);
        let colons: Vec<_> = toks
            .iter()
            .filter(|(t, _)| t == ":")
            .map(|(_, a)| a.colon)
            .collect();
        assert!(colons.contains(&ColonKind::BaseList));
        assert!(colons.contains(&ColonKind::SwitchLabel));
    }

    #[test]
    fn tolerates_broken_code() {
        let toks = annotate("class { void M( { ###");
        assert!(toks.iter().any(|(_, a)| a.in_error));
    }
}
