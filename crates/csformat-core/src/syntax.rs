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
    /// The token is an attribute bracket belonging to a declaration (not to a parameter).
    pub member_attribute: bool,
    /// The token is inside a `switch_section` (a `case` block).
    pub switch_section: bool,
    /// The enclosing switch section fits on a single source line.
    pub switch_section_inline: bool,
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
    /// Comma inside a multidimensional array rank (`new int[,]`).
    ArrayRank,
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
            if kind == "arrow_expression_clause" {
                info.flags |= flags::EXPRESSION_BODIED;
            }
            if nearest_decl.is_empty() && is_member_declaration(kind) {
                nearest_decl = kind;
            }
            if kind == "switch_section" && !info.switch_section {
                info.switch_section = true;
                info.switch_section_inline = source
                    .get(node.byte_range())
                    .is_none_or(|text| !text.contains('\n'));
            }
            if kind == "do_statement" && depth == 1 {
                // Only the terminating `;` of `do { } while (...);` is a direct child of the
                // do statement; the flag must not leak to statements inside its body.
                info.do_statement = true;
            }
            if is_member_declaration(kind) && node.start_byte() == token.start {
                info.member_start = true;
            }
            depth += 1;
            if depth > 64 || !cursor.goto_parent() {
                break;
            }
        }

        // Restore the cursor to the leaf so the next token can walk from here.
        while cursor.goto_first_child_for_byte(token.start).is_some() {}
        let leaf = cursor.node();

        info.brace = brace_kind(parent_kind);
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

fn brace_kind(parent: &str) -> BraceKind {
    match parent {
        "block" => BraceKind::Block,
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
        "array_rank_specifier" => CommaKind::ArrayRank,
        "initializer_expression" | "with_initializer" | "with_expression" => CommaKind::Initializer,
        "switch_expression" => CommaKind::SwitchExpression,
        "enum_member_declaration_list" => CommaKind::EnumBody,
        "tuple_expression" => CommaKind::Tuple,
        "collection_expression" => CommaKind::CollectionExpression,
        "type_argument_list" => CommaKind::TypeArguments,
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
