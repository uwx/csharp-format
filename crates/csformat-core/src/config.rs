//! Configuration resolution.
//!
//! Two configuration dialects are supported:
//!
//! * `.editorconfig`, including the Roslyn `csharp_*` formatting keys and the ReSharper
//!   `resharper_*` keys. Files are discovered from the filesystem root down to the source
//!   file, honouring `root = true`, and later/more specific values win.
//! * ReSharper `.DotSettings` XML files (`<s:String x:Key="/Default/CodeStyle/...">`),
//!   which are translated into the same internal options.
//!
//! Both dialects ultimately write into [`FormatOptions`]. Every applied key is recorded so
//! that `csharp-format config` can explain exactly where an option came from.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndentStyle {
    #[default]
    Space,
    Tab,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndOfLine {
    #[default]
    Auto,
    Lf,
    Crlf,
    Cr,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelIndent {
    #[default]
    OneLessThanCurrent,
    FlushLeft,
    NoChange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AroundBinary {
    #[default]
    BeforeAndAfter,
    None,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BraceCategories(pub u16);

/// ReSharper's `SpecialIndent` for preprocessor directives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialIndent {
    /// Column zero.
    #[default]
    NoIndent,
    /// Indented with the surrounding code.
    UsualIndent,
    /// One level less than the surrounding code.
    Outdent,
    /// Keep the column the directive was written at.
    DoNotChange,
}

/// ReSharper's `ParsFormatStyle`: how the content of a parenthesis is indented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParsIndent {
    /// BSD/K&R: the content indents one level, the closing `)` aligns with the owner.
    #[default]
    Inside,
    /// Whitesmiths: the `(` and its content share one indent level.
    Outside,
    /// GNU: the `(` at one level, the content at two.
    OutsideAndInside,
    /// No indent at all.
    None,
}

/// ReSharper's `AlignIndentDontChangeStyle` for raw string literals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignIndentStyle {
    #[default]
    Align,
    Indent,
    DoNotChange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BraceCategory {
    Control,
    Methods,
    Types,
    Properties,
    Accessors,
    AnonymousMethods,
    AnonymousTypes,
    Initializers,
    Lambdas,
    LocalFunctions,
}

impl BraceCategory {
    #[must_use]
    pub fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

impl BraceCategories {
    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self((1 << 10) - 1);

    #[must_use]
    pub fn contains(self, category: BraceCategory) -> bool {
        self.0 & category.bit() != 0
    }

    #[must_use]
    pub fn with(mut self, category: BraceCategory) -> Self {
        self.0 |= category.bit();
        self
    }

    #[must_use]
    pub fn is_all(self) -> bool {
        self.0 == Self::ALL.0
    }

    #[must_use]
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

impl Default for BraceCategories {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewlineOptions {
    pub before_open_brace: BraceCategories,
    pub before_else: bool,
    pub before_catch: bool,
    pub before_finally: bool,
    pub before_members_in_initializers: bool,
    pub before_members_in_anonymous_types: bool,
    pub between_query_clauses: bool,
}

impl Default for NewlineOptions {
    fn default() -> Self {
        Self {
            before_open_brace: BraceCategories::ALL,
            before_else: true,
            before_catch: true,
            before_finally: true,
            before_members_in_initializers: true,
            before_members_in_anonymous_types: true,
            between_query_clauses: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceOptions {
    pub after_cast: bool,
    pub before_colon_in_inheritance: bool,
    pub after_colon_in_inheritance: bool,
    pub after_comma: bool,
    pub before_comma: bool,
    pub after_dot: bool,
    pub before_dot: bool,
    pub after_keywords_in_control_flow: bool,
    pub after_semicolon_in_for: bool,
    pub before_semicolon_in_for: bool,
    pub around_binary_operators: AroundBinary,
    pub before_open_square_bracket: bool,
    pub between_empty_square_brackets: bool,
    pub between_square_brackets: bool,
    pub method_call_name_and_open_paren: bool,
    pub method_call_parameter_list_parentheses: bool,
    pub method_call_empty_parameter_list_parentheses: bool,
    pub method_declaration_name_and_open_paren: bool,
    pub method_declaration_parameter_list_parentheses: bool,
    pub method_declaration_empty_parameter_list_parentheses: bool,
    pub between_parentheses: bool,
    pub before_question_in_conditional: bool,
    pub after_question_in_conditional: bool,
    pub before_colon_in_conditional: bool,
    pub after_colon_in_conditional: bool,
    /// Maximum attribute section list length (excluding indentation) that still keeps the
    /// declaration on the same line. ReSharper's
    /// `resharper_csharp_max_attribute_length_for_same_line`.
    pub max_attribute_length_for_same_line: usize,
    /// Granular ReSharper `resharper_space_*` overrides, keyed by the part after
    /// `resharper_space_` (for example `around_additive_op`). The emitter falls back to
    /// ReSharper's default when a key is absent.
    pub resharper: BTreeMap<String, bool>,
}

impl Default for SpaceOptions {
    fn default() -> Self {
        Self {
            after_cast: false,
            before_colon_in_inheritance: true,
            after_colon_in_inheritance: true,
            after_comma: true,
            before_comma: false,
            after_dot: false,
            before_dot: false,
            after_keywords_in_control_flow: true,
            after_semicolon_in_for: true,
            before_semicolon_in_for: false,
            around_binary_operators: AroundBinary::BeforeAndAfter,
            before_open_square_bracket: false,
            between_empty_square_brackets: false,
            between_square_brackets: false,
            method_call_name_and_open_paren: false,
            method_call_parameter_list_parentheses: false,
            method_call_empty_parameter_list_parentheses: false,
            method_declaration_name_and_open_paren: false,
            method_declaration_parameter_list_parentheses: false,
            method_declaration_empty_parameter_list_parentheses: false,
            between_parentheses: false,
            before_question_in_conditional: true,
            after_question_in_conditional: true,
            before_colon_in_conditional: true,
            after_colon_in_conditional: true,
            max_attribute_length_for_same_line: 40,
            resharper: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlankLineOptions {
    /// Maximum number of consecutive blank lines preserved in code (method bodies).
    /// ReSharper's `resharper_csharp_keep_blank_lines_in_code`.
    pub keep_in_code: usize,
    /// Maximum number of consecutive blank lines preserved between declarations.
    /// ReSharper's `resharper_csharp_keep_blank_lines_in_declarations`.
    pub keep_in_declarations: usize,
    /// Remove blank lines next to braces in code (`remove_blank_lines_near_braces_in_code`).
    pub remove_near_braces_in_code: bool,
    /// Remove blank lines next to braces in declarations
    /// (`remove_blank_lines_near_braces_in_declarations`).
    pub remove_near_braces_in_declarations: bool,

    pub around_namespace: usize,
    pub inside_namespace: usize,
    pub after_file_scoped_namespace_directive: usize,
    pub around_type: usize,
    pub around_single_line_type: usize,
    pub inside_type: usize,

    pub around_field: usize,
    pub around_single_line_field: usize,
    pub around_property: usize,
    pub around_single_line_property: usize,
    pub around_auto_property: usize,
    pub around_single_line_auto_property: usize,
    pub around_accessor: usize,
    pub around_single_line_accessor: usize,
    pub around_invocable: usize,
    pub around_single_line_invocable: usize,
    pub around_local_method: usize,
    pub around_single_line_local_method: usize,

    pub around_region: usize,
    pub inside_region: usize,
    pub between_using_groups: usize,
    pub after_using_list: usize,
    pub after_start_comment: usize,
    pub before_single_line_comment: usize,

    pub before_control_transfer_statements: usize,
    pub after_control_transfer_statements: usize,
    pub before_block_statements: usize,
    pub after_block_statements: usize,
    pub before_multiline_statements: usize,
    pub after_multiline_statements: usize,
    pub before_case: usize,
    pub after_case: usize,
    pub around_multiline_case_section: usize,
    pub around_block_case_section: usize,

    /// Legacy key `resharper_blank_lines_between_members`: when set, force this many blank
    /// lines between members regardless of the per-kind `around_*` options.
    pub between_members: Option<usize>,
}

impl Default for BlankLineOptions {
    fn default() -> Self {
        Self {
            keep_in_code: 2,
            keep_in_declarations: 2,
            remove_near_braces_in_code: true,
            remove_near_braces_in_declarations: true,
            around_namespace: 1,
            inside_namespace: 0,
            after_file_scoped_namespace_directive: 1,
            around_type: 1,
            around_single_line_type: 1,
            inside_type: 0,
            around_field: 1,
            around_single_line_field: 0,
            around_property: 1,
            around_single_line_property: 0,
            around_auto_property: 1,
            around_single_line_auto_property: 0,
            around_accessor: 0,
            around_single_line_accessor: 0,
            around_invocable: 1,
            around_single_line_invocable: 0,
            around_local_method: 1,
            around_single_line_local_method: 0,
            around_region: 1,
            inside_region: 1,
            between_using_groups: 0,
            after_using_list: 1,
            after_start_comment: 1,
            before_single_line_comment: 0,
            before_control_transfer_statements: 0,
            after_control_transfer_statements: 0,
            before_block_statements: 0,
            after_block_statements: 1,
            before_multiline_statements: 0,
            after_multiline_statements: 0,
            before_case: 0,
            after_case: 0,
            around_multiline_case_section: 0,
            around_block_case_section: 0,
            between_members: None,
        }
    }
}

impl BlankLineOptions {
    /// Apply a blank-line setting by its suffix, e.g. `blank_lines_around_type`,
    /// `keep_blank_lines_in_code`, `remove_blank_lines_near_braces_in_declarations`.
    /// Returns `None` when the suffix is not part of this family, otherwise whether the
    /// value parsed.
    fn set_editorconfig(&mut self, suffix: &str, value: &str) -> Option<bool> {
        if let Some(rest) = suffix.strip_prefix("keep_blank_lines_") {
            match rest {
                "in_code" | "in_declarations" => {}
                _ => return None,
            }
            let Ok(count) = value.trim().parse::<usize>() else {
                return Some(false);
            };
            if rest == "in_code" {
                self.keep_in_code = count;
            } else {
                self.keep_in_declarations = count;
            }
            return Some(true);
        }
        if let Some(rest) = suffix.strip_prefix("remove_blank_lines_near_braces_") {
            match rest {
                "in_code" | "in_declarations" | "in_parent_languages" => {}
                _ => return None,
            }
            let Some(flag) = parse_bool(value) else {
                return Some(false);
            };
            match rest {
                "in_code" => self.remove_near_braces_in_code = flag,
                "in_declarations" => self.remove_near_braces_in_declarations = flag,
                // Parent-language braces do not occur in C# source.
                _ => {}
            }
            return Some(true);
        }
        let rest = suffix.strip_prefix("blank_lines_")?;
        // Legacy spellings used before ReSharper split the region options.
        let rest = match rest {
            "after_region" | "before_endregion" => "inside_region",
            other => other,
        };
        if rest == "between_members" {
            let Ok(count) = value.trim().parse::<usize>() else {
                return Some(false);
            };
            self.between_members = Some(count);
            return Some(true);
        }
        let slot = match rest {
            "around_namespace" => &mut self.around_namespace,
            "inside_namespace" => &mut self.inside_namespace,
            "after_file_scoped_namespace_directive" => {
                &mut self.after_file_scoped_namespace_directive
            }
            "around_type" => &mut self.around_type,
            "around_single_line_type" => &mut self.around_single_line_type,
            "inside_type" => &mut self.inside_type,
            "around_field" => &mut self.around_field,
            "around_single_line_field" => &mut self.around_single_line_field,
            "around_property" => &mut self.around_property,
            "around_single_line_property" => &mut self.around_single_line_property,
            "around_auto_property" => &mut self.around_auto_property,
            "around_single_line_auto_property" => &mut self.around_single_line_auto_property,
            "around_accessor" => &mut self.around_accessor,
            "around_single_line_accessor" => &mut self.around_single_line_accessor,
            "around_invocable" => &mut self.around_invocable,
            "around_single_line_invocable" => &mut self.around_single_line_invocable,
            "around_local_method" => &mut self.around_local_method,
            "around_single_line_local_method" => &mut self.around_single_line_local_method,
            "around_region" => &mut self.around_region,
            "inside_region" => &mut self.inside_region,
            "between_using_groups" => &mut self.between_using_groups,
            "after_using_list" => &mut self.after_using_list,
            "after_start_comment" => &mut self.after_start_comment,
            "before_single_line_comment" => &mut self.before_single_line_comment,
            "before_control_transfer_statements" => &mut self.before_control_transfer_statements,
            "after_control_transfer_statements" => &mut self.after_control_transfer_statements,
            "before_block_statements" => &mut self.before_block_statements,
            "after_block_statements" => &mut self.after_block_statements,
            "before_multiline_statements" => &mut self.before_multiline_statements,
            "after_multiline_statements" => &mut self.after_multiline_statements,
            "before_case" => &mut self.before_case,
            "after_case" => &mut self.after_case,
            "around_multiline_case_section" => &mut self.around_multiline_case_section,
            "around_block_case_section" => &mut self.around_block_case_section,
            _ => return None,
        };
        let Ok(count) = value.trim().parse::<usize>() else {
            return Some(false);
        };
        *slot = count;
        Some(true)
    }

    /// Apply a `.DotSettings` blank-line entry by its upper-case field name.
    fn set_dotsettings(&mut self, name: &str, value: &str) -> Option<bool> {
        self.set_editorconfig(&name.to_ascii_lowercase(), value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapStyle {
    /// Fill lines up to the width limit (`wrap_if_long`, ReSharper's default).
    #[default]
    WrapIfLong,
    /// If the construct does not fit (or already spans lines), put every element on
    /// its own line (`chop_if_long`).
    ChopIfLong,
    /// Always put every element on its own line (`chop_always`).
    ChopAlways,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrapOptions {
    /// Master switch. ReSharper's `resharper_wrap_lines`.
    pub enabled: bool,
    /// Maximum line length. `None` disables hard wrapping.
    pub max_length: Option<usize>,
    /// Extra indentation used for wrapped continuation lines.
    pub continuation_indent: usize,

    /// `resharper_csharp_wrap_arguments_style`.
    pub arguments: WrapStyle,
    /// `resharper_csharp_wrap_parameters_style`.
    pub parameters: WrapStyle,
    /// `resharper_csharp_wrap_primary_constructor_parameters_style`.
    pub primary_constructor_parameters: WrapStyle,
    /// `resharper_csharp_wrap_extends_list_style`.
    pub extends_list: WrapStyle,
    /// `resharper_csharp_wrap_object_and_collection_initializer_style`.
    pub object_initializer: WrapStyle,
    /// `resharper_csharp_wrap_array_initializer_style`.
    pub array_initializer: WrapStyle,
    /// `resharper_csharp_wrap_enum_declaration`.
    pub enum_declaration: WrapStyle,
    /// `resharper_csharp_wrap_switch_expression`.
    pub switch_expression: WrapStyle,
    /// `resharper_csharp_wrap_list_pattern`.
    pub list_pattern: WrapStyle,
    /// `resharper_csharp_wrap_property_pattern`.
    pub property_pattern: WrapStyle,
    /// `resharper_csharp_wrap_for_stmt_header_style`.
    pub for_header: WrapStyle,
    /// `resharper_csharp_wrap_multiple_declaration_style`.
    pub multiple_declaration: WrapStyle,
    /// `resharper_csharp_wrap_multiple_type_parameter_constraints_style`.
    pub multiple_constraints: WrapStyle,

    /// `resharper_csharp_wrap_before_comma`.
    pub before_comma: bool,
    /// `resharper_csharp_wrap_before_eq`.
    pub before_eq: bool,
    /// `resharper_csharp_wrap_before_binary_opsign`.
    pub before_binary_opsign: bool,
    /// `resharper_csharp_wrap_before_ternary_opsigns`.
    pub before_ternary_opsigns: bool,
    /// `resharper_csharp_wrap_before_extends_colon`.
    pub before_extends_colon: bool,
    /// `resharper_csharp_wrap_before_type_parameter_langle`.
    pub before_type_parameter_langle: bool,
    /// `resharper_csharp_wrap_before_first_type_parameter_constraint`.
    pub before_first_type_parameter_constraint: bool,
    /// `resharper_csharp_wrap_before_arrow_with_expressions`.
    pub before_arrow_with_expressions: bool,
    /// `resharper_csharp_wrap_before_linq_expression`.
    pub before_linq_expression: bool,
    /// `resharper_csharp_wrap_after_invocation_lpar`.
    pub after_invocation_lpar: bool,
    /// `resharper_csharp_wrap_before_invocation_rpar`.
    pub before_invocation_rpar: bool,
    /// `resharper_csharp_wrap_after_declaration_lpar`.
    pub after_declaration_lpar: bool,
    /// `resharper_csharp_wrap_before_declaration_rpar`.
    pub before_declaration_rpar: bool,
    /// `resharper_csharp_wrap_after_primary_constructor_declaration_lpar`.
    pub after_primary_constructor_declaration_lpar: bool,
    /// `resharper_csharp_wrap_before_primary_constructor_declaration_rpar`.
    pub before_primary_constructor_declaration_rpar: bool,
    /// `resharper_csharp_wrap_chained_binary_expressions`.
    pub chained_binary_expressions: bool,
    /// `resharper_csharp_wrap_chained_binary_patterns`.
    pub chained_binary_patterns: bool,
    /// `resharper_csharp_wrap_chained_method_calls`.
    pub chained_method_calls: bool,
    /// `resharper_csharp_wrap_after_dot_in_method_calls`.
    pub after_dot_in_method_calls: bool,
    /// `resharper_csharp_wrap_before_first_method_call`.
    pub before_first_method_call: bool,
    /// `resharper_csharp_wrap_after_property_in_chained_method_calls`.
    pub after_property_in_chained_method_calls: bool,
    /// `resharper_csharp_wrap_linq_expressions`.
    pub linq_expressions: bool,

    /// `resharper_csharp_max_invocation_arguments_on_line` (0 = no limit).
    pub max_arguments_on_line: Option<usize>,
    /// `resharper_csharp_max_formal_parameters_on_line`.
    pub max_formal_parameters_on_line: Option<usize>,
    /// `resharper_csharp_max_primary_constructor_parameters_on_line`.
    pub max_primary_constructor_parameters_on_line: Option<usize>,
    /// `resharper_csharp_max_initializer_elements_on_line`.
    pub max_initializer_elements_on_line: Option<usize>,
    /// `resharper_csharp_max_array_initializer_elements_on_line`.
    pub max_array_initializer_elements_on_line: Option<usize>,
    /// `resharper_csharp_max_enum_members_on_line`.
    pub max_enum_members_on_line: Option<usize>,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            max_length: Some(120),
            continuation_indent: 1,
            arguments: WrapStyle::WrapIfLong,
            parameters: WrapStyle::WrapIfLong,
            primary_constructor_parameters: WrapStyle::ChopIfLong,
            extends_list: WrapStyle::WrapIfLong,
            object_initializer: WrapStyle::WrapIfLong,
            array_initializer: WrapStyle::WrapIfLong,
            enum_declaration: WrapStyle::WrapIfLong,
            switch_expression: WrapStyle::WrapIfLong,
            list_pattern: WrapStyle::WrapIfLong,
            property_pattern: WrapStyle::WrapIfLong,
            for_header: WrapStyle::WrapIfLong,
            multiple_declaration: WrapStyle::WrapIfLong,
            multiple_constraints: WrapStyle::WrapIfLong,
            before_comma: false,
            before_eq: false,
            before_binary_opsign: false,
            before_ternary_opsigns: false,
            before_extends_colon: false,
            before_type_parameter_langle: false,
            before_first_type_parameter_constraint: false,
            before_arrow_with_expressions: false,
            before_linq_expression: false,
            after_invocation_lpar: false,
            before_invocation_rpar: false,
            after_declaration_lpar: false,
            before_declaration_rpar: false,
            after_primary_constructor_declaration_lpar: true,
            before_primary_constructor_declaration_rpar: false,
            chained_binary_expressions: false,
            chained_binary_patterns: false,
            chained_method_calls: true,
            after_dot_in_method_calls: false,
            before_first_method_call: false,
            after_property_in_chained_method_calls: false,
            linq_expressions: false,
            max_arguments_on_line: None,
            max_formal_parameters_on_line: None,
            max_primary_constructor_parameters_on_line: None,
            max_initializer_elements_on_line: None,
            max_array_initializer_elements_on_line: None,
            max_enum_members_on_line: None,
        }
    }
}

/// ReSharper's `align_*` settings: which continuation lines are aligned under the first
/// element of their construct instead of indented one level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignOptions {
    pub first_arg_by_paren: bool,
    pub linq_query: bool,
    pub multiline_argument: bool,
    pub multiline_array_and_object_initializer: bool,
    pub multiline_binary_expressions_chain: bool,
    pub multiline_binary_patterns: bool,
    pub multiline_calls_chain: bool,
    pub multiline_comments: bool,
    pub multiline_expression: bool,
    pub multiline_extends_list: bool,
    pub multiline_for_stmt: bool,
    pub multiline_list_pattern: bool,
    pub multiline_parameter: bool,
    pub multiline_property_pattern: bool,
    pub multiline_statement_conditions: bool,
    pub multiline_switch_expression: bool,
    pub multiple_declaration: bool,
    pub multiline_type_parameter_constrains: bool,
    pub multiline_type_parameter_list: bool,
    pub tuple_components: bool,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self {
            first_arg_by_paren: false,
            linq_query: false,
            multiline_argument: false,
            multiline_array_and_object_initializer: false,
            multiline_binary_expressions_chain: true,
            multiline_binary_patterns: false,
            multiline_calls_chain: false,
            multiline_comments: true,
            multiline_expression: false,
            multiline_extends_list: false,
            multiline_for_stmt: false,
            multiline_list_pattern: false,
            multiline_parameter: false,
            multiline_property_pattern: false,
            multiline_statement_conditions: true,
            multiline_switch_expression: false,
            multiple_declaration: false,
            multiline_type_parameter_constrains: false,
            multiline_type_parameter_list: false,
            tuple_components: false,
        }
    }
}

impl AlignOptions {
    /// Apply an `align_*` setting by its suffix. Returns `None` when the suffix is not part
    /// of this family, otherwise whether the value parsed.
    fn set_resharper(&mut self, suffix: &str, value: &str) -> Option<bool> {
        let Some(flag) = parse_bool(value) else {
            return Some(false);
        };
        let slot = match suffix {
            "first_arg_by_paren" => &mut self.first_arg_by_paren,
            "linq_query" => &mut self.linq_query,
            "multiline_argument" => &mut self.multiline_argument,
            "multiline_array_and_object_initializer" => {
                &mut self.multiline_array_and_object_initializer
            }
            "multiline_binary_expressions_chain" => &mut self.multiline_binary_expressions_chain,
            "multiline_binary_patterns" => &mut self.multiline_binary_patterns,
            "multiline_calls_chain" => &mut self.multiline_calls_chain,
            "multiline_comments" => &mut self.multiline_comments,
            "multiline_expression" => &mut self.multiline_expression,
            "multiline_extends_list" => &mut self.multiline_extends_list,
            "multiline_for_stmt" => &mut self.multiline_for_stmt,
            "multiline_list_pattern" => &mut self.multiline_list_pattern,
            "multiline_parameter" => &mut self.multiline_parameter,
            "multiline_property_pattern" => &mut self.multiline_property_pattern,
            "multiline_statement_conditions" => &mut self.multiline_statement_conditions,
            "multiline_switch_expression" => &mut self.multiline_switch_expression,
            "multiple_declaration" => &mut self.multiple_declaration,
            "multline_type_parameter_constrains" | "multiline_type_parameter_constrains" => {
                &mut self.multiline_type_parameter_constrains
            }
            "multline_type_parameter_list" | "multiline_type_parameter_list" => {
                &mut self.multiline_type_parameter_list
            }
            "tuple_components" => &mut self.tuple_components,
            _ => return None,
        };
        *slot = flag;
        Some(true)
    }
}

/// ReSharper's `indent_*` / `outdent_*` / `continuous_indent_*` settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndentOptions {
    pub inside_namespace: bool,
    pub anonymous_method_block: bool,
    pub braces_inside_statement_conditions: bool,
    pub nested_usings: bool,
    pub nested_fixed: bool,
    pub nested_lock: bool,
    pub nested_for: bool,
    pub nested_foreach: bool,
    pub nested_while: bool,
    pub type_constraints: bool,
    pub method_decl_pars: ParsIndent,
    pub primary_constructor_decl_pars: ParsIndent,
    pub invocation_pars: ParsIndent,
    pub statement_pars: ParsIndent,
    pub typeparam_angles: ParsIndent,
    pub typearg_angles: ParsIndent,
    pub pars: ParsIndent,
    pub preprocessor_if: SpecialIndent,
    pub preprocessor_region: SpecialIndent,
    pub preprocessor_other: SpecialIndent,
    pub raw_literal_string: AlignIndentStyle,
    pub use_continuous_indent_inside_parens: bool,
    pub use_continuous_indent_inside_initializer_braces: bool,
    pub outdent_binary_ops: bool,
    pub outdent_binary_pattern_ops: bool,
    pub outdent_commas: bool,
    pub outdent_dots: bool,
    pub outdent_statement_labels: bool,
    pub continuous_indent_multiplier: usize,
}

impl Default for IndentOptions {
    fn default() -> Self {
        Self {
            inside_namespace: true,
            anonymous_method_block: false,
            braces_inside_statement_conditions: true,
            nested_usings: false,
            nested_fixed: false,
            nested_lock: false,
            nested_for: false,
            nested_foreach: false,
            nested_while: false,
            type_constraints: true,
            method_decl_pars: ParsIndent::Inside,
            primary_constructor_decl_pars: ParsIndent::Inside,
            invocation_pars: ParsIndent::Inside,
            statement_pars: ParsIndent::Inside,
            typeparam_angles: ParsIndent::Inside,
            typearg_angles: ParsIndent::Inside,
            pars: ParsIndent::Inside,
            preprocessor_if: SpecialIndent::NoIndent,
            preprocessor_region: SpecialIndent::UsualIndent,
            preprocessor_other: SpecialIndent::NoIndent,
            raw_literal_string: AlignIndentStyle::Align,
            use_continuous_indent_inside_parens: true,
            use_continuous_indent_inside_initializer_braces: true,
            outdent_binary_ops: false,
            outdent_binary_pattern_ops: false,
            outdent_commas: false,
            outdent_dots: false,
            outdent_statement_labels: false,
            continuous_indent_multiplier: 1,
        }
    }
}

impl IndentOptions {
    /// Apply an `indent_*` / `outdent_*` / `continuous_indent_*` setting by its suffix.
    /// Returns `None` when the suffix is not part of this family, otherwise whether the
    /// value parsed.
    fn set_resharper(&mut self, suffix: &str, value: &str) -> Option<bool> {
        let v = value.trim().to_ascii_lowercase();
        if suffix == "continuous_indent_multiplier" {
            let Ok(count) = v.parse::<usize>() else {
                return Some(false);
            };
            self.continuous_indent_multiplier = count;
            return Some(true);
        }
        let flag = |slot: &mut bool| match parse_bool(value) {
            Some(flag) => {
                *slot = flag;
                Some(true)
            }
            None => Some(false),
        };
        let special = |slot: &mut SpecialIndent| {
            *slot = match v.as_str() {
                "no_indent" | "none" => SpecialIndent::NoIndent,
                "usual_indent" | "usual" => SpecialIndent::UsualIndent,
                "outdent" => SpecialIndent::Outdent,
                "do_not_change" => SpecialIndent::DoNotChange,
                _ => return Some(false),
            };
            Some(true)
        };
        let pars = |slot: &mut ParsIndent| {
            *slot = match v.as_str() {
                "inside" | "inside_parenthesis" => ParsIndent::Inside,
                "outside" => ParsIndent::Outside,
                "outside_and_inside" => ParsIndent::OutsideAndInside,
                "none" | "no_indent" => ParsIndent::None,
                _ => return Some(false),
            };
            Some(true)
        };
        Some(match suffix {
            "indent_inside_namespace" => return flag(&mut self.inside_namespace),
            "indent_anonymous_method_block" => return flag(&mut self.anonymous_method_block),
            "indent_braces_inside_statement_conditions" => {
                return flag(&mut self.braces_inside_statement_conditions);
            }
            "indent_nested_usings_stmt" => return flag(&mut self.nested_usings),
            "indent_nested_fixed_stmt" => return flag(&mut self.nested_fixed),
            "indent_nested_lock_stmt" => return flag(&mut self.nested_lock),
            "indent_nested_for_stmt" => return flag(&mut self.nested_for),
            "indent_nested_foreach_stmt" => return flag(&mut self.nested_foreach),
            "indent_nested_while_stmt" => return flag(&mut self.nested_while),
            "indent_type_constraints" => return flag(&mut self.type_constraints),
            "use_continuous_indent_inside_parens" => {
                return flag(&mut self.use_continuous_indent_inside_parens);
            }
            "use_continuous_indent_inside_initializer_braces" => {
                return flag(&mut self.use_continuous_indent_inside_initializer_braces);
            }
            "outdent_binary_ops" => return flag(&mut self.outdent_binary_ops),
            "outdent_binary_pattern_ops" => return flag(&mut self.outdent_binary_pattern_ops),
            "outdent_commas" => return flag(&mut self.outdent_commas),
            "outdent_dots" => return flag(&mut self.outdent_dots),
            "outdent_statement_labels" => return flag(&mut self.outdent_statement_labels),
            "indent_method_decl_pars" => return pars(&mut self.method_decl_pars),
            "indent_primary_constructor_decl_pars" => {
                return pars(&mut self.primary_constructor_decl_pars);
            }
            "indent_invocation_pars" => return pars(&mut self.invocation_pars),
            "indent_statement_pars" => return pars(&mut self.statement_pars),
            "indent_typeparam_angles" => return pars(&mut self.typeparam_angles),
            "indent_typearg_angles" => return pars(&mut self.typearg_angles),
            "indent_pars" => return pars(&mut self.pars),
            "indent_preprocessor_if" => return special(&mut self.preprocessor_if),
            "indent_preprocessor_region" => return special(&mut self.preprocessor_region),
            "indent_preprocessor_other" => return special(&mut self.preprocessor_other),
            "indent_raw_literal_string" => {
                self.raw_literal_string = match v.as_str() {
                    "align" => AlignIndentStyle::Align,
                    "indent" => AlignIndentStyle::Indent,
                    "do_not_change" => AlignIndentStyle::DoNotChange,
                    _ => return Some(false),
                };
                true
            }
            _ => return None,
        })
    }
}

/// Everything the layout engine needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatOptions {
    pub indent_style: IndentStyle,
    pub indent_size: usize,
    pub tab_width: usize,
    pub continuation_indent_size: usize,
    pub end_of_line: EndOfLine,
    pub insert_final_newline: bool,
    pub trim_trailing_whitespace: bool,
    pub prefer_bom: Option<bool>,
    pub max_line_length: Option<usize>,
    pub newline: NewlineOptions,
    pub space: SpaceOptions,
    pub blank_lines: BlankLineOptions,
    pub indent: IndentOptions,
    pub align: AlignOptions,
    pub wrap: WrapOptions,
    pub preserve_single_line_blocks: bool,
    pub preserve_single_line_statements: bool,
    pub indent_case_contents: bool,
    pub indent_switch_labels: bool,
    pub indent_braces: bool,
    pub indent_labels: LabelIndent,
    /// `dotnet_sort_system_directives_first`; `None` means do not reorder usings at all.
    pub sort_usings: Option<bool>,
    pub separate_using_groups: Option<bool>,
    /// Diagnostics produced while resolving configuration.
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_style: IndentStyle::Space,
            indent_size: 4,
            tab_width: 4,
            continuation_indent_size: 4,
            end_of_line: EndOfLine::Auto,
            insert_final_newline: false,
            trim_trailing_whitespace: true,
            prefer_bom: None,
            max_line_length: None,
            newline: NewlineOptions::default(),
            space: SpaceOptions::default(),
            blank_lines: BlankLineOptions::default(),
            indent: IndentOptions::default(),
            align: AlignOptions::default(),
            wrap: WrapOptions::default(),
            preserve_single_line_blocks: true,
            preserve_single_line_statements: true,
            indent_case_contents: true,
            indent_switch_labels: true,
            indent_braces: false,
            indent_labels: LabelIndent::NoChange,
            sort_usings: None,
            separate_using_groups: None,
            warnings: Vec::new(),
        }
    }
}

impl FormatOptions {
    #[must_use]
    pub fn indent_unit(&self) -> IndentUnit {
        match self.indent_style {
            IndentStyle::Space => IndentUnit::Spaces(self.indent_size.max(1)),
            IndentStyle::Tab => IndentUnit::Tabs,
        }
    }

    /// The effective continuation indent, in columns, for space-indented documents.
    #[must_use]
    pub fn continuation_indent(&self) -> usize {
        self.continuation_indent_size.max(self.indent_size).max(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentUnit {
    Spaces(usize),
    Tabs,
}

#[derive(Clone, Debug)]
pub struct AppliedSetting {
    pub source: PathBuf,
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    pub options: FormatOptions,
    pub sources: Vec<PathBuf>,
    pub applied: Vec<AppliedSetting>,
}

impl ResolvedConfig {
    #[must_use]
    pub fn with_defaults() -> Self {
        Self {
            options: FormatOptions::default(),
            sources: Vec::new(),
            applied: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// EditorConfig parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct EditorConfigFile {
    root: bool,
    sections: Vec<Section>,
}

#[derive(Debug, Default)]
struct Section {
    patterns: Vec<String>,
    properties: Vec<(String, String)>,
}

/// Split a section header's pattern list on commas that are not inside braces,
/// so `[*.{cs,csx}, *.txt]` yields `*.{cs,csx}` and `*.txt`.
fn split_patterns(header: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (idx, ch) in header.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                push_pattern(&mut patterns, &header[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    push_pattern(&mut patterns, &header[start..]);
    patterns
}

fn push_pattern(patterns: &mut Vec<String>, raw: &str) {
    let pattern = raw.trim();
    if !pattern.is_empty() {
        patterns.push(pattern.to_owned());
    }
}

fn parse_editorconfig(text: &str) -> EditorConfigFile {
    let mut file = EditorConfigFile::default();
    let mut current: Option<Section> = None;
    let mut in_preamble = true;
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_preamble = false;
            if let Some(section) = current.take() {
                file.sections.push(section);
            }
            let end = line.rfind(']').unwrap_or(line.len());
            let patterns = split_patterns(&line[1..end]);
            current = Some(Section {
                patterns,
                properties: Vec::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if in_preamble {
            if key == "root" {
                file.root = value.eq_ignore_ascii_case("true");
            }
            continue;
        }
        if let Some(section) = current.as_mut() {
            section.properties.push((key, value));
        }
    }
    if let Some(section) = current {
        file.sections.push(section);
    }
    file
}

#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let pattern = pattern.replace('\\', "/");
    let pattern = pattern.trim_start_matches('/');
    let candidate = if pattern.contains('/') {
        &path
    } else {
        path.rsplit('/').next().unwrap_or(&path)
    };
    for expanded in expand_braces(pattern) {
        if glob_regex(&expanded).is_match(candidate) {
            return true;
        }
    }
    false
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    // Find the matching close brace, allowing nesting.
    let mut depth = 0usize;
    let mut close = None;
    for (idx, ch) in pattern[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + idx);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![pattern.to_owned()];
    };
    let prefix = &pattern[..open];
    let body = &pattern[open + 1..close];
    let suffix = &pattern[close + 1..];
    // Split the body on top-level commas.
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (idx, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&body[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&body[start..]);
    let mut out = Vec::new();
    for part in parts {
        for rest in expand_braces(suffix) {
            for inner in expand_braces(&format!("{prefix}{part}{rest}")) {
                out.push(inner);
            }
        }
    }
    if out.is_empty() {
        vec![pattern.to_owned()]
    } else {
        out
    }
}

fn glob_regex(pattern: &str) -> regex::Regex {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    if chars.get(i + 2) == Some(&'/') {
                        re.push_str("(?:.*/)?");
                        i += 3;
                    } else {
                        re.push_str(".*");
                        i += 2;
                    }
                } else {
                    re.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                re.push_str("[^/]");
                i += 1;
            }
            '[' => {
                // Character class, with `!` as negation and `]` allowed first.
                let mut j = i + 1;
                let mut negated = false;
                if chars.get(j) == Some(&'!') || chars.get(j) == Some(&'^') {
                    negated = true;
                    j += 1;
                }
                let class_start = j;
                let mut class = String::new();
                if chars.get(j) == Some(&']') {
                    class.push_str("\\]");
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    let ch = chars[j];
                    if ch == '\\' {
                        class.push_str("\\\\");
                    } else {
                        class.push(ch);
                    }
                    j += 1;
                }
                if j < chars.len() && j > class_start {
                    re.push('[');
                    if negated {
                        re.push('^');
                    }
                    re.push_str(&class);
                    re.push(']');
                    i = j + 1;
                } else {
                    re.push_str("\\[");
                    i += 1;
                }
            }
            ch => {
                if "\\^$.*+?()|{}".contains(ch) {
                    re.push('\\');
                }
                re.push(ch);
                i += 1;
            }
        }
    }
    re.push('$');
    regex::RegexBuilder::new(&re)
        .case_insensitive(cfg!(windows))
        .build()
        .unwrap_or_else(|_| regex::Regex::new("$^").expect("valid regex"))
}

// ---------------------------------------------------------------------------------------------
// DotSettings parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct DotSettings {
    entries: Vec<(String, String)>,
}

fn parse_dotsettings(path: &Path) -> DotSettings {
    use quick_xml::events::Event;
    let Ok(text) = std::fs::read_to_string(path) else {
        return DotSettings::default();
    };
    let mut reader = quick_xml::Reader::from_str(&text);
    reader.config_mut().trim_text(false);
    let mut entries = Vec::new();
    let mut current_key: Option<String> = None;
    let mut buffer = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let mut key = None;
                for attr in e.attributes().flatten() {
                    let name = String::from_utf8_lossy(attr.key.as_ref());
                    if name == "x:Key" || name.ends_with(":Key") || name == "Key" {
                        key = Some(String::from_utf8_lossy(&attr.value).into_owned());
                    }
                }
                if let Some(key) = key {
                    if let Some(previous) = current_key.take() {
                        entries.push((previous, buffer.trim().to_owned()));
                    }
                    buffer.clear();
                    current_key = Some(key);
                }
            }
            Ok(Event::Text(e)) => {
                if current_key.is_some() {
                    buffer.push_str(&String::from_utf8_lossy(&e));
                }
            }
            Ok(Event::End(_)) => {
                if let Some(key) = current_key.take() {
                    entries.push((key, buffer.trim().to_owned()));
                    buffer.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    DotSettings { entries }
}

// ---------------------------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------------------------

pub struct ConfigResolver {
    editorconfigs: Mutex<HashMap<PathBuf, Arc<EditorConfigFile>>>,
    dotsettings: Mutex<HashMap<PathBuf, Arc<DotSettings>>>,
    directory_scan: Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>,
}

impl Default for ConfigResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            editorconfigs: Mutex::new(HashMap::new()),
            dotsettings: Mutex::new(HashMap::new()),
            directory_scan: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve configuration for `file`. The file need not exist for path matching, but
    /// configuration files are discovered relative to it.
    #[must_use]
    pub fn resolve(&self, file: &Path) -> ResolvedConfig {
        let editorconfigs = self.find_editorconfigs(file);
        let dotsettings = self.find_dotsettings(file);
        let mut options = FormatOptions::default();
        let mut applied = Vec::new();
        let mut sources = Vec::new();

        for path in &editorconfigs {
            let cfg = self.load_editorconfig(path);
            sources.push(path.clone());
            let relative = path
                .parent()
                .and_then(|dir| file.strip_prefix(dir).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| file.to_string_lossy().into_owned());
            for section in &cfg.sections {
                if !section
                    .patterns
                    .iter()
                    .any(|pattern| glob_match(pattern, &relative))
                {
                    continue;
                }
                for (key, value) in &section.properties {
                    apply_key(&mut options, key, value, path, &mut applied);
                }
            }
        }

        for path in &dotsettings {
            let settings = self.load_dotsettings(path);
            sources.push(path.clone());
            for (key, value) in &settings.entries {
                apply_dotsettings_key(&mut options, key, value, path, &mut applied);
            }
        }
        dedup_warnings(&mut options.warnings);
        ResolvedConfig {
            options,
            sources,
            applied,
        }
    }

    fn find_editorconfigs(&self, file: &Path) -> Vec<PathBuf> {
        let mut result = Vec::new();
        let start = absolute_parent(file);
        let mut dir = Some(start.as_path());
        while let Some(current) = dir {
            let path = current.join(".editorconfig");
            if path.is_file() {
                let parsed = self.load_editorconfig(&path);
                let is_root = parsed.root;
                result.push(path);
                if is_root {
                    break;
                }
            }
            dir = current.parent();
        }
        result.reverse();
        result
    }

    fn find_dotsettings(&self, file: &Path) -> Vec<PathBuf> {
        let mut result = Vec::new();
        let start = absolute_parent(file);
        let mut dir = Some(start.as_path());
        while let Some(current) = dir {
            let entries = self.scan_dir(current);
            for path in entries.iter() {
                if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.ends_with(".DotSettings") || n.ends_with(".DotSettings.user")
                }) {
                    result.push(path.clone());
                }
            }
            dir = current.parent();
        }
        result.reverse();
        result
    }

    fn scan_dir(&self, dir: &Path) -> Arc<Vec<PathBuf>> {
        let mut cache = self
            .directory_scan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = cache.get(dir) {
            return Arc::clone(found);
        }
        let paths = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let paths = Arc::new(paths);
        cache.insert(dir.to_path_buf(), Arc::clone(&paths));
        paths
    }

    fn load_editorconfig(&self, path: &Path) -> Arc<EditorConfigFile> {
        let mut cache = self
            .editorconfigs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = cache.get(path) {
            return Arc::clone(found);
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let parsed = Arc::new(parse_editorconfig(&text));
        cache.insert(path.to_path_buf(), Arc::clone(&parsed));
        parsed
    }

    fn load_dotsettings(&self, path: &Path) -> Arc<DotSettings> {
        let mut cache = self
            .dotsettings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = cache.get(path) {
            return Arc::clone(found);
        }
        let parsed = Arc::new(parse_dotsettings(path));
        cache.insert(path.to_path_buf(), Arc::clone(&parsed));
        parsed
    }
}

fn absolute_parent(file: &Path) -> PathBuf {
    let absolute = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(file)
    };
    absolute
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn dedup_warnings(warnings: &mut Vec<String>) {
    let mut seen = HashSet::new();
    warnings.retain(|w| seen.insert(w.clone()));
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn set_bool(
    slot: &mut bool,
    value: &str,
    key: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
    warnings: &mut Vec<String>,
) {
    if let Some(v) = parse_bool(value) {
        *slot = v;
        applied.push(AppliedSetting {
            source: path.to_path_buf(),
            key: key.to_owned(),
            value: value.to_owned(),
        });
    } else {
        warnings.push(format!(
            "{}: invalid boolean for {key}: {value:?}",
            path.display()
        ));
    }
}

fn set_usize(
    slot: &mut usize,
    value: &str,
    key: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
    warnings: &mut Vec<String>,
) {
    match value.trim().parse::<usize>() {
        Ok(v) => {
            *slot = v;
            applied.push(AppliedSetting {
                source: path.to_path_buf(),
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
        Err(_) => warnings.push(format!(
            "{}: invalid integer for {key}: {value:?}",
            path.display()
        )),
    }
}

fn set_max_on_line(
    slot: &mut Option<usize>,
    value: &str,
    key: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
    warnings: &mut Vec<String>,
) {
    match value.trim().parse::<usize>() {
        Ok(0) => {
            *slot = None;
            applied.push(AppliedSetting {
                source: path.to_path_buf(),
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
        Ok(v) => {
            *slot = Some(v);
            applied.push(AppliedSetting {
                source: path.to_path_buf(),
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
        Err(_) => warnings.push(format!(
            "{}: invalid integer for {key}: {value:?}",
            path.display()
        )),
    }
}

fn set_wrap_style(
    slot: &mut WrapStyle,
    value: &str,
    key: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
    warnings: &mut Vec<String>,
) {
    set_enum(slot, value, key, path, applied, warnings, |v| match v {
        "wrap_if_long" | "simple_wrap" => Some(WrapStyle::WrapIfLong),
        "chop_if_long" => Some(WrapStyle::ChopIfLong),
        "chop_always" => Some(WrapStyle::ChopAlways),
        _ => None,
    });
}

fn set_enum<T: Copy>(
    slot: &mut T,
    value: &str,
    key: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
    warnings: &mut Vec<String>,
    map: impl Fn(&str) -> Option<T>,
) {
    if let Some(v) = map(&value.trim().to_ascii_lowercase()) {
        *slot = v;
        applied.push(AppliedSetting {
            source: path.to_path_buf(),
            key: key.to_owned(),
            value: value.to_owned(),
        });
    } else {
        warnings.push(format!(
            "{}: unsupported value for {key}: {value:?}",
            path.display()
        ));
    }
}

pub fn apply_key(
    options: &mut FormatOptions,
    key: &str,
    value: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
) {
    let mut key = key.trim().to_ascii_lowercase();
    // ReSharper has language-specific aliases such as `resharper_csharp_brace_style`;
    // our handlers are C#-specific, so collapse them onto the shared `resharper_` form.
    if let Some(rest) = key.strip_prefix("resharper_csharp_") {
        key = format!("resharper_{rest}");
    } else if let Some(rest) = key.strip_prefix("csharp_wrap_") {
        // ReSharper also documents the prefix-less form, e.g.
        // `csharp_wrap_arguments_style`.
        key = format!("resharper_wrap_{rest}");
    }
    let value_trim = value.trim();
    let v = value_trim.to_ascii_lowercase();
    let mut push = |field: &str| {
        applied.push(AppliedSetting {
            source: path.to_path_buf(),
            key: field.to_owned(),
            value: value_trim.to_owned(),
        });
    };

    if let Some(rest) = key.strip_prefix("resharper_align_") {
        match options.align.set_resharper(rest, value_trim) {
            Some(true) => push(&key),
            Some(false) => options.warnings.push(format!(
                "{}: invalid value for {key}: {value_trim:?}",
                path.display()
            )),
            None => options.warnings.push(format!(
                "{}: unsupported align option {key}",
                path.display()
            )),
        }
        return;
    }

    if let Some(rest) = key
        .strip_prefix("resharper_indent_")
        .map(|rest| format!("indent_{rest}"))
        .or_else(|| {
            key.strip_prefix("resharper_outdent_")
                .map(|rest| format!("outdent_{rest}"))
        })
        .or_else(|| {
            key.strip_prefix("resharper_continuous_indent_")
                .map(|rest| format!("continuous_indent_{rest}"))
        })
        .or_else(|| {
            key.strip_prefix("resharper_use_continuous_")
                .map(|rest| format!("use_continuous_{rest}"))
        })
    {
        if matches!(rest.as_str(), "indent_size" | "indent_style") {
            // Handled by the general indent arms below.
        } else {
            match options.indent.set_resharper(&rest, value_trim) {
                Some(true) => push(&key),
                Some(false) => options.warnings.push(format!(
                    "{}: invalid value for {key}: {value_trim:?}",
                    path.display()
                )),
                None => options.warnings.push(format!(
                    "{}: unsupported indent option {key}",
                    path.display()
                )),
            }
            return;
        }
    }

    if let Some(rest) = key.strip_prefix("resharper_space_") {
        match parse_bool(value_trim) {
            Some(v) => {
                options.space.resharper.insert(rest.to_owned(), v);
                push(&key);
            }
            None => options.warnings.push(format!(
                "{}: invalid boolean for {key}: {value_trim:?}",
                path.display()
            )),
        }
        return;
    }

    match key.as_str() {
        "indent_style" | "resharper_indent_style" => match v.as_str() {
            "space" => {
                options.indent_style = IndentStyle::Space;
                push(&key);
            }
            "tab" => {
                options.indent_style = IndentStyle::Tab;
                push(&key);
            }
            _ => options.warnings.push(format!(
                "{}: invalid indent_style {value_trim:?}",
                path.display()
            )),
        },
        "indent_size" | "resharper_indent_size" => {
            if v == "tab" {
                options.indent_size = options.tab_width;
                push(&key);
            } else {
                set_usize(
                    &mut options.indent_size,
                    value_trim,
                    &key,
                    path,
                    applied,
                    &mut options.warnings,
                );
            }
        }
        "tab_width" => set_usize(
            &mut options.tab_width,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "continuation_indent_size" | "resharper_continuation_indent_size" => {
            set_usize(
                &mut options.continuation_indent_size,
                value_trim,
                &key,
                path,
                applied,
                &mut options.warnings,
            );
        }
        "end_of_line" => set_enum(
            &mut options.end_of_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
            |v| match v {
                "lf" => Some(EndOfLine::Lf),
                "crlf" => Some(EndOfLine::Crlf),
                "cr" => Some(EndOfLine::Cr),
                _ => None,
            },
        ),
        "insert_final_newline" => set_bool(
            &mut options.insert_final_newline,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "trim_trailing_whitespace" => set_bool(
            &mut options.trim_trailing_whitespace,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "charset" => {
            let bom = match v.as_str() {
                "utf-8" => Some(false),
                "utf-8-bom" => Some(true),
                _ => None,
            };
            if let Some(bom) = bom {
                options.prefer_bom = Some(bom);
                push(&key);
            } else {
                options.warnings.push(format!(
                    "{}: charset {value_trim:?} is not supported",
                    path.display()
                ));
            }
        }
        "max_line_length" | "resharper_max_line_length" => {
            if v == "off" || v == "unset" {
                options.max_line_length = None;
                push(&key);
            } else {
                let mut n = 0usize;
                set_usize(
                    &mut n,
                    value_trim,
                    &key,
                    path,
                    applied,
                    &mut options.warnings,
                );
                if n > 0 {
                    options.max_line_length = Some(n);
                    options.wrap.max_length = Some(n);
                }
            }
        }
        "resharper_wrap_lines" => {
            let mut b = options.wrap.enabled;
            set_bool(
                &mut b,
                value_trim,
                &key,
                path,
                applied,
                &mut options.warnings,
            );
            options.wrap.enabled = b;
        }
        "csharp_new_line_before_open_brace" => {
            if let Some(categories) = parse_brace_categories(value_trim) {
                options.newline.before_open_brace = categories;
                push(&key);
            } else {
                options.warnings.push(format!(
                    "{}: unsupported csharp_new_line_before_open_brace value {value_trim:?}",
                    path.display()
                ));
            }
        }
        "csharp_new_line_before_else" => set_bool(
            &mut options.newline.before_else,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_new_line_before_catch" => set_bool(
            &mut options.newline.before_catch,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_new_line_before_finally" => set_bool(
            &mut options.newline.before_finally,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_new_line_before_members_in_object_initializers"
        | "csharp_new_line_before_members_in_anonymous_types" => {
            let slot = if key.contains("anonymous") {
                &mut options.newline.before_members_in_anonymous_types
            } else {
                &mut options.newline.before_members_in_initializers
            };
            set_bool(slot, value_trim, &key, path, applied, &mut options.warnings);
        }
        "csharp_new_line_between_query_expression_clauses" => set_bool(
            &mut options.newline.between_query_clauses,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_preserve_single_line_blocks" => set_bool(
            &mut options.preserve_single_line_blocks,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_preserve_single_line_statements" => set_bool(
            &mut options.preserve_single_line_statements,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_indent_case_contents" => set_bool(
            &mut options.indent_case_contents,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_indent_switch_labels" => set_bool(
            &mut options.indent_switch_labels,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_indent_braces" => set_bool(
            &mut options.indent_braces,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_indent_labels" => set_enum(
            &mut options.indent_labels,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
            |v| match v {
                "one_less_than_current" => Some(LabelIndent::OneLessThanCurrent),
                "flush_left" => Some(LabelIndent::FlushLeft),
                "no_change" => Some(LabelIndent::NoChange),
                _ => None,
            },
        ),
        "csharp_space_after_cast" => set_bool(
            &mut options.space.after_cast,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_before_colon_in_inheritance_clause" => set_bool(
            &mut options.space.before_colon_in_inheritance,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_colon_in_inheritance_clause" => set_bool(
            &mut options.space.after_colon_in_inheritance,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_comma" => set_bool(
            &mut options.space.after_comma,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_before_comma" => set_bool(
            &mut options.space.before_comma,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_dot" => set_bool(
            &mut options.space.after_dot,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_before_dot" => set_bool(
            &mut options.space.before_dot,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_keywords_in_control_flow_statements" => set_bool(
            &mut options.space.after_keywords_in_control_flow,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_semicolon_in_for_statement" => set_bool(
            &mut options.space.after_semicolon_in_for,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_before_semicolon_in_for_statement" => set_bool(
            &mut options.space.before_semicolon_in_for,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_around_binary_operators" => set_enum(
            &mut options.space.around_binary_operators,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
            |v| match v {
                "before_and_after" => Some(AroundBinary::BeforeAndAfter),
                "none" => Some(AroundBinary::None),
                "ignore" => Some(AroundBinary::Ignore),
                _ => None,
            },
        ),
        "csharp_space_before_open_square_brackets" => set_bool(
            &mut options.space.before_open_square_bracket,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_empty_square_brackets" => set_bool(
            &mut options.space.between_empty_square_brackets,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_square_brackets" => set_bool(
            &mut options.space.between_square_brackets,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_method_call_name_and_opening_parenthesis" => set_bool(
            &mut options.space.method_call_name_and_open_paren,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_method_call_parameter_list_parentheses" => set_bool(
            &mut options.space.method_call_parameter_list_parentheses,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_method_call_empty_parameter_list_parentheses" => set_bool(
            &mut options.space.method_call_empty_parameter_list_parentheses,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_method_declaration_name_and_open_parenthesis" => set_bool(
            &mut options.space.method_declaration_name_and_open_paren,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_method_declaration_parameter_list_parentheses" => set_bool(
            &mut options.space.method_declaration_parameter_list_parentheses,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_method_declaration_empty_parameter_list_parentheses" => set_bool(
            &mut options
                .space
                .method_declaration_empty_parameter_list_parentheses,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_between_parentheses" => set_bool(
            &mut options.space.between_parentheses,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_before_question_in_conditional" => set_bool(
            &mut options.space.before_question_in_conditional,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_question_in_conditional" => set_bool(
            &mut options.space.after_question_in_conditional,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_before_colon_in_conditional" => set_bool(
            &mut options.space.before_colon_in_conditional,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "csharp_space_after_colon_in_conditional" => set_bool(
            &mut options.space.after_colon_in_conditional,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "dotnet_sort_system_directives_first" => {
            let mut b = options.sort_usings.unwrap_or(true);
            set_bool(
                &mut b,
                value_trim,
                &key,
                path,
                applied,
                &mut options.warnings,
            );
            options.sort_usings = Some(b);
        }
        "dotnet_separate_import_directive_groups" => {
            let mut b = options.separate_using_groups.unwrap_or(false);
            set_bool(
                &mut b,
                value_trim,
                &key,
                path,
                applied,
                &mut options.warnings,
            );
            options.separate_using_groups = Some(b);
        }
        "resharper_blank_lines_between_members" => {
            let mut n = 0usize;
            set_usize(
                &mut n,
                value_trim,
                &key,
                path,
                applied,
                &mut options.warnings,
            );
            options.blank_lines.between_members = Some(n);
        }
        "resharper_max_attribute_length_for_same_line"
        | "csharp_max_attribute_length_for_same_line" => {
            let mut n = options.space.max_attribute_length_for_same_line;
            set_usize(
                &mut n,
                value_trim,
                &key,
                path,
                applied,
                &mut options.warnings,
            );
            options.space.max_attribute_length_for_same_line = n;
        }
        "resharper_brace_style" => set_enum(
            &mut options.newline.before_open_brace,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
            |v| match v {
                "next_line" | "allman" => Some(BraceCategories::ALL),
                "end_of_line" | "k_r" | "kr" | "none" => Some(BraceCategories::NONE),
                "next_line_when_multiline" | "next_line_shifted" => None,
                _ => None,
            },
        ),
        "resharper_wrap_arguments_style" => set_wrap_style(
            &mut options.wrap.arguments,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_parameters_style" => set_wrap_style(
            &mut options.wrap.parameters,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_primary_constructor_parameters_style" => set_wrap_style(
            &mut options.wrap.primary_constructor_parameters,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_extends_list_style" => set_wrap_style(
            &mut options.wrap.extends_list,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_object_and_collection_initializer_style" => set_wrap_style(
            &mut options.wrap.object_initializer,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_array_initializer_style" => set_wrap_style(
            &mut options.wrap.array_initializer,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_enum_declaration" => set_wrap_style(
            &mut options.wrap.enum_declaration,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_switch_expression" => set_wrap_style(
            &mut options.wrap.switch_expression,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_list_pattern" => set_wrap_style(
            &mut options.wrap.list_pattern,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_property_pattern" => set_wrap_style(
            &mut options.wrap.property_pattern,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_for_stmt_header_style" => set_wrap_style(
            &mut options.wrap.for_header,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_multiple_declaration_style" => set_wrap_style(
            &mut options.wrap.multiple_declaration,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_multiple_type_parameter_constraints_style" => set_wrap_style(
            &mut options.wrap.multiple_constraints,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_comma" => set_bool(
            &mut options.wrap.before_comma,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_eq" => set_bool(
            &mut options.wrap.before_eq,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_binary_opsign" => set_bool(
            &mut options.wrap.before_binary_opsign,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_ternary_opsigns" => set_bool(
            &mut options.wrap.before_ternary_opsigns,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_extends_colon" => set_bool(
            &mut options.wrap.before_extends_colon,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_type_parameter_langle" => set_bool(
            &mut options.wrap.before_type_parameter_langle,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_first_type_parameter_constraint" => set_bool(
            &mut options.wrap.before_first_type_parameter_constraint,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_arrow_with_expressions" => set_bool(
            &mut options.wrap.before_arrow_with_expressions,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_linq_expression" => set_bool(
            &mut options.wrap.before_linq_expression,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_after_invocation_lpar" => set_bool(
            &mut options.wrap.after_invocation_lpar,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_invocation_rpar" => set_bool(
            &mut options.wrap.before_invocation_rpar,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_after_declaration_lpar" => set_bool(
            &mut options.wrap.after_declaration_lpar,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_declaration_rpar" => set_bool(
            &mut options.wrap.before_declaration_rpar,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_after_primary_constructor_declaration_lpar" => set_bool(
            &mut options.wrap.after_primary_constructor_declaration_lpar,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_primary_constructor_declaration_rpar" => set_bool(
            &mut options.wrap.before_primary_constructor_declaration_rpar,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_chained_binary_expressions" => set_bool(
            &mut options.wrap.chained_binary_expressions,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_chained_binary_patterns" => set_bool(
            &mut options.wrap.chained_binary_patterns,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_chained_method_calls" => set_bool(
            &mut options.wrap.chained_method_calls,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_after_dot_in_method_calls" => set_bool(
            &mut options.wrap.after_dot_in_method_calls,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_before_first_method_call" => set_bool(
            &mut options.wrap.before_first_method_call,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_after_property_in_chained_method_calls" => set_bool(
            &mut options.wrap.after_property_in_chained_method_calls,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_linq_expressions" => set_bool(
            &mut options.wrap.linq_expressions,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_max_invocation_arguments_on_line" => set_max_on_line(
            &mut options.wrap.max_arguments_on_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_max_formal_parameters_on_line" => set_max_on_line(
            &mut options.wrap.max_formal_parameters_on_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_max_primary_constructor_parameters_on_line" => set_max_on_line(
            &mut options.wrap.max_primary_constructor_parameters_on_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_max_initializer_elements_on_line" => set_max_on_line(
            &mut options.wrap.max_initializer_elements_on_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_max_array_initializer_elements_on_line" => set_max_on_line(
            &mut options.wrap.max_array_initializer_elements_on_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_max_enum_members_on_line" => set_max_on_line(
            &mut options.wrap.max_enum_members_on_line,
            value_trim,
            &key,
            path,
            applied,
            &mut options.warnings,
        ),
        "resharper_wrap_after_delegates" => {
            // Accepted for compatibility; delegates are wrapped with their invocation.
            options.warnings.push(format!(
                "{key} is accepted but not yet granularly implemented"
            ));
        }
        _ => {
            if let Some(rest) = key.strip_prefix("resharper_") {
                if (rest.starts_with("blank_lines_")
                    || rest.starts_with("keep_blank_lines_")
                    || rest.starts_with("remove_blank_lines_"))
                    && let Some(ok) = options.blank_lines.set_editorconfig(rest, value_trim)
                {
                    if ok {
                        push(&key);
                    } else {
                        options.warnings.push(format!(
                            "{}: invalid value for {key}: {value_trim:?}",
                            path.display()
                        ));
                    }
                    return;
                }
            }
            let whitespace_key = key.starts_with("csharp_space_")
                || key.starts_with("csharp_new_line_")
                || key.starts_with("csharp_indent_")
                || key.starts_with("dotnet_")
                || key.starts_with("resharper_space_")
                || key.starts_with("resharper_new_line_")
                || key.starts_with("resharper_indent_")
                || key.starts_with("resharper_wrap_")
                || key.starts_with("resharper_align_")
                || key.starts_with("resharper_blank_lines_")
                || key.starts_with("resharper_min_blank_lines_")
                || key.starts_with("resharper_keep_blank_lines_")
                || key.starts_with("resharper_max_");
            if whitespace_key {
                // Every whitespace key is at least parsed; the ones without a dedicated
                // handler are reported as accepted-but-coarse instead of unknown.
                options.warnings.push(format!(
                    "{key} is accepted but not yet granularly implemented"
                ));
                applied.push(AppliedSetting {
                    source: path.to_path_buf(),
                    key: key.clone(),
                    value: value_trim.to_owned(),
                });
            }
        }
    }
}

fn parse_brace_categories(value: &str) -> Option<BraceCategories> {
    let mut categories = BraceCategories::NONE;
    for part in value.split(',') {
        let part = part.trim().to_ascii_lowercase();
        match part.as_str() {
            "all" => return Some(BraceCategories::ALL),
            "none" => return Some(BraceCategories::NONE),
            "methods" => categories = categories.with(BraceCategory::Methods),
            "types" => categories = categories.with(BraceCategory::Types),
            "properties" => categories = categories.with(BraceCategory::Properties),
            "accessors" => categories = categories.with(BraceCategory::Accessors),
            "anonymous_methods" => categories = categories.with(BraceCategory::AnonymousMethods),
            "anonymous_types" => categories = categories.with(BraceCategory::AnonymousTypes),
            "object_collection_array_initializers" | "initializers" => {
                categories = categories.with(BraceCategory::Initializers);
            }
            "lambdas" => categories = categories.with(BraceCategory::Lambdas),
            "local_functions" => categories = categories.with(BraceCategory::LocalFunctions),
            "control_blocks" | "control" => categories = categories.with(BraceCategory::Control),
            "" => {}
            _ => return None,
        }
    }
    Some(categories)
}

fn apply_dotsettings_key(
    options: &mut FormatOptions,
    key: &str,
    value: &str,
    path: &Path,
    applied: &mut Vec<AppliedSetting>,
) {
    if value.is_empty() {
        return;
    }
    let Some(name) = key.rsplit('/').next() else {
        return;
    };
    let name = name.trim().to_ascii_uppercase();
    let source = path.to_path_buf();
    let mut push = |field: &str, value: &str| {
        applied.push(AppliedSetting {
            source: source.clone(),
            key: field.to_owned(),
            value: value.to_owned(),
        });
    };
    let v = value.trim().to_ascii_lowercase();
    match name.as_str() {
        "INDENT_SIZE" => {
            if let Ok(n) = value.trim().parse::<usize>() {
                options.indent_size = n;
                push(&name, value);
            }
        }
        "TAB_WIDTH" => {
            if let Ok(n) = value.trim().parse::<usize>() {
                options.tab_width = n;
                push(&name, value);
            }
        }
        "TABS_AND_INDENTS" => {
            if let Some(true) = parse_bool(value) {
                options.indent_style = IndentStyle::Tab;
                push(&name, value);
            }
        }
        "CONTINUATION_INDENT_SIZE" => {
            if let Ok(n) = value.trim().parse::<usize>() {
                options.continuation_indent_size = n;
                push(&name, value);
            }
        }
        "MAX_LINE_LENGTH" => {
            if let Ok(n) = value.trim().parse::<usize>() {
                if n > 0 {
                    options.max_line_length = Some(n);
                    options.wrap.max_length = Some(n);
                }
                push(&name, value);
            }
        }
        "WRAP_LINES" => {
            if let Some(b) = parse_bool(value) {
                options.wrap.enabled = b;
                push(&name, value);
            }
        }
        "BRACE_STYLE" => {
            let categories = match v.as_str() {
                "next_line" => Some(BraceCategories::ALL),
                "end_of_line" => Some(BraceCategories::NONE),
                _ => None,
            };
            if let Some(categories) = categories {
                options.newline.before_open_brace = categories;
                push(&name, value);
            }
        }
        "NEW_LINE_BEFORE_ELSE" => {
            if let Some(b) = parse_bool(value) {
                options.newline.before_else = b;
                push(&name, value);
            }
        }
        "NEW_LINE_BEFORE_CATCH" => {
            if let Some(b) = parse_bool(value) {
                options.newline.before_catch = b;
                push(&name, value);
            }
        }
        "NEW_LINE_BEFORE_FINALLY" => {
            if let Some(b) = parse_bool(value) {
                options.newline.before_finally = b;
                push(&name, value);
            }
        }
        "BLANK_LINES_BETWEEN_MEMBERS" => {
            if let Ok(n) = value.trim().parse::<usize>() {
                options.blank_lines.between_members = Some(n);
                push(&name, value);
            }
        }
        "SPACE_AFTER_CAST" => {
            if let Some(b) = parse_bool(value) {
                options.space.after_cast = b;
                push(&name, value);
            }
        }
        "SPACE_AFTER_COMMA" => {
            if let Some(b) = parse_bool(value) {
                options.space.after_comma = b;
                push(&name, value);
            }
        }
        "SPACE_BEFORE_COMMA" => {
            if let Some(b) = parse_bool(value) {
                options.space.before_comma = b;
                push(&name, value);
            }
        }
        "SPACE_AROUND_BINARY_OPERATORS" => {
            let mode = match v.as_str() {
                "true" | "before_and_after" => Some(AroundBinary::BeforeAndAfter),
                "false" | "none" => Some(AroundBinary::None),
                "ignore" => Some(AroundBinary::Ignore),
                _ => None,
            };
            if let Some(mode) = mode {
                options.space.around_binary_operators = mode;
                push(&name, value);
            }
        }
        "OUTDENT_SWITCH_LABELS" => {
            if let Some(b) = parse_bool(value) {
                options.indent_switch_labels = !b;
                push(&name, value);
            }
        }
        "OUTDENT_CASE_CONTENTS" => {
            if let Some(b) = parse_bool(value) {
                options.indent_case_contents = !b;
                push(&name, value);
            }
        }
        "SORT_USINGS" => {
            if let Some(b) = parse_bool(value) {
                options.sort_usings = Some(b);
                push(&name, value);
            }
        }
        "SYSTEM_USINGS_FIRST" => {
            let b = parse_bool(value).unwrap_or(true);
            options.sort_usings = Some(b);
            push(&name, value);
        }
        _ => {
            let blank_lines = name.starts_with("BLANK_LINES_")
                || name.starts_with("KEEP_BLANK_LINES_")
                || name.starts_with("REMOVE_BLANK_LINES_NEAR_BRACES_");
            if blank_lines && let Some(ok) = options.blank_lines.set_dotsettings(&name, value) {
                if ok {
                    push(&name, value);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.cs", "Foo.cs"));
        assert!(glob_match("*.cs", "src/deep/Foo.cs"));
        assert!(glob_match("**/*.cs", "src/deep/Foo.cs"));
        assert!(glob_match("**/*.cs", "Foo.cs"));
        assert!(glob_match("*.{cs,csx}", "Foo.csx"));
        assert!(glob_match("src/**/*.cs", "src/a/b/Foo.cs"));
        assert!(!glob_match("src/*.cs", "src/a/Foo.cs"));
        assert!(glob_match("[Tt]est?.cs", "Test1.cs"));
    }

    #[test]
    fn parses_editorconfig() {
        let text =
            "root = true\n\n[*.cs]\nindent_size = 2\ncsharp_new_line_before_open_brace = all\n";
        let file = parse_editorconfig(text);
        assert!(file.root);
        assert_eq!(file.sections.len(), 1);
        assert_eq!(file.sections[0].patterns, vec!["*.cs"]);
    }

    #[test]
    fn applies_roslyn_space_keys() {
        let mut options = FormatOptions::default();
        let mut applied = Vec::new();
        apply_key(
            &mut options,
            "csharp_space_after_cast",
            "true",
            Path::new("/x/.editorconfig"),
            &mut applied,
        );
        assert!(options.space.after_cast);
        apply_key(
            &mut options,
            "csharp_space_around_binary_operators",
            "none",
            Path::new("/x/.editorconfig"),
            &mut applied,
        );
        assert_eq!(options.space.around_binary_operators, AroundBinary::None);
    }

    #[test]
    fn parses_dotsettings() {
        let dir = std::env::temp_dir().join("csformat-dotsettings-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Test.DotSettings");
        std::fs::write(
            &path,
            r#"<wpf:ResourceDictionary xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" xmlns:s="clr-namespace:System;assembly=mscorlib">
  <s:String x:Key="/Default/CodeStyle/CodeFormatting/CSharpFormat/INDENT_SIZE">2</s:String>
  <s:Boolean x:Key="/Default/CodeStyle/CodeFormatting/CSharpFormat/USE_TABS">True</s:Boolean>
</wpf:ResourceDictionary>"#,
        )
        .unwrap();
        let parsed = parse_dotsettings(&path);
        assert_eq!(parsed.entries.len(), 2);
        assert!(
            parsed
                .entries
                .iter()
                .any(|(k, v)| k.ends_with("INDENT_SIZE") && v == "2")
        );
    }
}
