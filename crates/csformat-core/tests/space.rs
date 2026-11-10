//! Granular ReSharper `space_*` behaviour, verified against the ReSharper CLI
//! (`Built-in: Reformat Code`, ReSharper 2026.2).

use std::path::Path;

use csformat_core::config::apply_key;
use csformat_core::{FormatOptions, format_source};

fn options(keys: &[(&str, bool)]) -> FormatOptions {
    let mut options = FormatOptions::default();
    options.insert_final_newline = false;
    let mut applied = Vec::new();
    for (key, value) in keys {
        apply_key(
            &mut options,
            &format!("resharper_csharp_space_{key}"),
            if *value { "true" } else { "false" },
            Path::new(".editorconfig"),
            &mut applied,
        );
    }
    options
}

fn format(source: &str, keys: &[(&str, bool)]) -> String {
    format_source(source, &options(keys)).output
}

#[test]
fn before_semicolon_applies_after_every_statement() {
    let source = "class C\n{\n    void M()\n    {\n        var z = 1;\n    }\n}";
    let expected = "class C\n{\n    void M()\n    {\n        var z = 1 ;\n    }\n}";
    assert_eq!(format(source, &[("before_semicolon", true)]), expected);
    assert_eq!(format(source, &[]), source);
}

#[test]
fn using_alias_equals_follows_around_alias_eq() {
    let source = "using Alias = System.Console;\nusing System;";
    let expected = "using Alias=System.Console ;\nusing System ;";
    assert_eq!(
        format(
            source,
            &[("around_alias_eq", false), ("before_semicolon", true)]
        ),
        expected
    );
}

#[test]
fn type_argument_and_parameter_angles() {
    let source = "class Box<T1, T2>\n{\n    List<int> items = new List<int>();\n}";
    let expected = "class Box < T1, T2 >\n{\n    List < int > items = new List < int >() ;\n}";
    assert_eq!(
        format(
            source,
            &[
                ("before_type_argument_angle", true),
                ("within_type_argument_angles", true),
                ("before_type_parameter_angle", true),
                ("within_type_parameter_angles", true),
                ("before_semicolon", true),
            ],
        ),
        expected
    );
}

#[test]
fn pointer_declaration_keeps_a_space_after_the_star() {
    let source = "class C\n{\n    int* pField;\n\n    unsafe void M()\n    {\n        int* p = null;\n    }\n}";
    let expected = "class C\n{\n    int * pField ;\n\n    unsafe void M()\n    {\n        int * p = null ;\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("before_pointer_asterik_declaration", true),
                ("before_semicolon", true),
            ],
        ),
        expected
    );
}

#[test]
fn keyword_and_type_preference_glues_ref() {
    let source = "class C\n{\n    void M()\n    {\n        Take(ref (int, int) a);\n    }\n\n    void Take(ref (int, int) v) { }\n}";
    let expected = "class C\n{\n    void M()\n    {\n        Take(ref(int, int) a) ;\n    }\n\n    void Take(ref(int, int) v)\n    {\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("between_keyword_and_type", false),
                ("before_semicolon", true)
            ],
        ),
        expected
    );
}

#[test]
fn keyword_and_expression_preference_glues_return_and_throw() {
    let source = "class C\n{\n    (int, int) M()\n    {\n        throw (new Exception());\n        return (1, 2);\n    }\n}";
    let expected = "class C\n{\n    (int, int) M()\n    {\n        throw(new Exception()) ;\n        return(1, 2) ;\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("between_keyword_and_expression", false),
                ("before_semicolon", true),
            ],
        ),
        expected
    );
}

#[test]
fn lambda_arrow_spacing_is_separate_from_the_pointer_arrow() {
    let source = "class C\n{\n    void M()\n    {\n        Func<int, int> f = q => q + 1;\n        var s = ptr->ToString();\n    }\n}";
    let expected = "class C\n{\n    void M()\n    {\n        Func<int, int> f = q=>q + 1;\n        var s = ptr->ToString();\n    }\n}";
    assert_eq!(format(source, &[("around_lambda_arrow", false)]), expected);
}

#[test]
fn trailing_comment_space_and_attribute_gluing() {
    let source = "class C\n{\n    void M(int x)\n    {\n        var z = x; // trailing\n    }\n}";
    let expected = "class C\n{\n    void M(int x)\n    {\n        var z = x ;// trailing\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("before_trailing_comment", false),
                ("before_semicolon", true)
            ],
        ),
        expected
    );

    let attributes = "class C\n{\n    void M([NotNull] [ItemNotNull] string s)\n    {\n    }\n}";
    let expected = "class C\n{\n    void M([NotNull][ItemNotNull]string s)\n    {\n    }\n}";
    assert_eq!(
        format(
            attributes,
            &[
                ("after_attributes", false),
                ("between_attribute_sections", false),
            ],
        ),
        expected
    );
}

#[test]
fn unary_and_postfix_operator_spacing() {
    let source = "class C\n{\n    void M(int b)\n    {\n        int c = -b;\n        int d = !b;\n        int e = b++;\n    }\n}";
    let expected = "class C\n{\n    void M(int b)\n    {\n        int c = - b ;\n        int d = ! b ;\n        int e = b ++ ;\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("after_unary_minus_op", true),
                ("after_logical_not_op", true),
                ("near_postfix_and_prefix_op", true),
                ("before_semicolon", true),
            ],
        ),
        expected
    );
}

#[test]
fn constraint_colon_preferences() {
    let source = "class C\n{\n    void M<T1>() where T1 : IComparable<T1>\n    {\n    }\n}";
    assert_eq!(format(source, &[]), source);
    let expected = "class C\n{\n    void M<T1>() where T1:IComparable<T1>\n    {\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("before_type_parameter_constraint_colon", false),
                ("after_type_parameter_constraint_colon", false),
            ],
        ),
        expected
    );
}

#[test]
fn cast_is_glued_to_the_call_it_converts() {
    let source = "class C\n{\n    void M()\n    {\n        var r = (int, int)Method2();\n    }\n\n    (int, int) Method2()\n    {\n        return (1, 2);\n    }\n}";
    let expected = "class C\n{\n    void M()\n    {\n        var r = (int, int)Method2();\n    }\n\n    (int, int) Method2()\n    {\n        return (1, 2);\n    }\n}";
    assert_eq!(format(source, &[]), expected);
}

#[test]
fn empty_braces_and_accessors() {
    let source = "class C\n{\n    int P\n    {\n        get { }\n        set { }\n    }\n\n    int Auto { get; set; }\n}";
    assert_eq!(
        format(source, &[]),
        source,
        "`within_empty_braces` defaults on"
    );

    let expected = "class C\n{\n    int P\n    {\n        get {}\n        set {}\n    }\n\n    int Auto{get ;set ;}\n}";
    assert_eq!(
        format(
            source,
            &[
                ("within_empty_braces", false),
                ("before_singleline_accessorholder", false),
                ("in_singleline_accessorholder", false),
                ("between_accessors_in_singleline_property", false),
                ("before_semicolon", true),
            ],
        ),
        expected
    );
}

#[test]
fn slice_pattern_spacing() {
    let source = "class C\n{\n    void M(int[] a)\n    {\n        var t = a is [1, 2, .. var tail];\n    }\n}";
    assert_eq!(format(source, &[]), source);
    let expected = "class C\n{\n    void M(int[] a)\n    {\n        var t = a is [1, 2, ..var tail];\n    }\n}";
    assert_eq!(format(source, &[("within_slice_pattern", false)]), expected);
}

#[test]
fn nameof_and_new_parentheses() {
    let source = "class C\n{\n    void M()\n    {\n        var a = nameof(M);\n        List<int> x = new List<int>();\n    }\n}";
    let expected = "class C\n{\n    void M()\n    {\n        var a = nameof ( M );\n        List < int > x = new List < int >();\n    }\n}";
    assert_eq!(
        format(
            source,
            &[
                ("before_nameof_parentheses", true),
                ("within_nameof_parentheses", true),
                ("before_type_argument_angle", true),
                ("within_type_argument_angles", true),
            ],
        ),
        expected
    );
}
