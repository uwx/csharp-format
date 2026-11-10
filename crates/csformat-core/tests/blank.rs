//! Granular ReSharper `blank_lines_*` behaviour, verified against the ReSharper CLI
//! (`Built-in: Reformat Code`, ReSharper 2026.2).

use std::path::Path;

use csformat_core::config::apply_key;
use csformat_core::{FormatOptions, format_source};

fn options(keys: &[&str]) -> FormatOptions {
    let mut options = FormatOptions::default();
    options.insert_final_newline = false;
    let mut applied = Vec::new();
    for entry in keys {
        let (key, value) = entry.split_once('=').expect("key = value");
        apply_key(
            &mut options,
            &format!("resharper_csharp_{}", key.trim()),
            value.trim(),
            Path::new(".editorconfig"),
            &mut applied,
        );
    }
    options
}

fn format(source: &str, keys: &[&str]) -> String {
    format_source(source, &options(keys)).output
}

/// Formats `source` and compares it with `expected`, ignoring the final newline (the
/// tests keep `insert_final_newline` off).
fn check(source: &str, keys: &[&str], expected: &str) {
    assert_eq!(
        format(source, keys),
        expected.trim_end_matches('\n'),
        "keys: {keys:?}"
    );
}

#[test]
fn keep_blank_lines_caps_at_two_by_default() {
    let source = "class A\n{\n    int f;\n\n\n\n\n    int g;\n}\n";
    let expected = "class A\n{\n    int f;\n\n\n    int g;\n}\n";
    check(source, &[], expected);
}

#[test]
fn keep_blank_lines_in_code_zero_collapses_gaps() {
    let source = "class A\n{\n    void M()\n    {\n        int x;\n\n\n        int y;\n    }\n}\n";
    let expected = "class A\n{\n    void M()\n    {\n        int x;\n        int y;\n    }\n}\n";
    check(
        source,
        &[
            "keep_blank_lines_in_code = 0",
            "keep_blank_lines_in_declarations = 0",
        ],
        expected,
    );
}

#[test]
fn keeping_blank_lines_near_braces_can_be_disabled() {
    let source = "class A\n{\n    void M()\n    {\n\n        int x = 1;\n    }\n}\n";
    check(
        source,
        &["remove_blank_lines_near_braces_in_code = false"],
        source,
    );
    let removed = "class A\n{\n    void M()\n    {\n        int x = 1;\n    }\n}\n";
    check(source, &[], removed);
}

#[test]
fn blank_line_after_using_list() {
    let source = "using System;\nusing System.Linq;\nclass C\n{\n}\n";
    let expected = "using System;\nusing System.Linq;\n\nclass C\n{\n}\n";
    check(source, &[], expected);
    // The `around_type` minimum still applies, so `after_using_list = 0` keeps one blank.
    check(source, &["blank_lines_after_using_list = 0"], expected);
    check(
        source,
        &["blank_lines_after_using_list = 2"],
        "using System;
using System.Linq;


class C
{
}
",
    );
}

#[test]
fn using_groups_follow_static_flag_and_first_segment() {
    let source = "using System;\nusing System.Linq;\nusing static System.Math;\nusing A;\n";
    let expected = "using System;\nusing System.Linq;\n\nusing static System.Math;\n\nusing A;\n";
    check(source, &["blank_lines_between_using_groups = 1"], expected);
    check(source, &[], source);
}

#[test]
fn using_aliases_form_a_group_of_their_own() {
    let source = "using System;\nusing X = System.Text.StringBuilder;\nusing Y = System.Text.StringBuilder;\n";
    let expected = "using System;\n\nusing X = System.Text.StringBuilder;\nusing Y = System.Text.StringBuilder;\n";
    check(source, &["blank_lines_between_using_groups = 1"], expected);
}

#[test]
fn blank_lines_after_file_scoped_namespace_directive() {
    let source = "namespace N;\nusing System;\n";
    check(source, &[], "namespace N;\n\nusing System;\n");
    check(
        source,
        &["blank_lines_after_file_scoped_namespace_directive = 2"],
        "namespace N;\n\n\nusing System;\n",
    );
    check(
        source,
        &["blank_lines_after_file_scoped_namespace_directive = 0"],
        source,
    );
}

#[test]
fn element_lists_never_keep_blank_lines() {
    let source = "class A\n{\n    void M(int a,\n\n        int b)\n    {\n        M(a,\n\n            b);\n        var d = new Dictionary<int,\n\n            string>();\n        var x = xs[a,\n\n            b];\n    }\n}\n";
    let expected = "class A\n{\n    void M(int a,\n        int b)\n    {\n        M(a,\n            b);\n        var d = new Dictionary<int,\n            string>();\n        var x = xs[a,\n            b];\n    }\n}\n";
    check(source, &[], expected);
    check(
        source,
        &["remove_blank_lines_near_braces_in_code = false"],
        expected,
    );
}

#[test]
fn initializers_keep_blank_lines_between_their_elements() {
    let source = "class A\n{\n    void M()\n    {\n        int[] a =\n        [\n            1,\n\n            2\n        ];\n    }\n}\n";
    check(source, &[], source);
}

#[test]
fn multiline_members_are_separated_by_a_blank_line() {
    let source = "class A\n{\n    int f;\n    void M()\n    {\n    }\n}\n";
    let expected = "class A\n{\n    int f;\n\n    void M()\n    {\n    }\n}\n";
    check(source, &[], expected);
    check(source, &["blank_lines_around_invocable = 0"], source);
}

#[test]
fn a_bare_block_statement_is_not_separated() {
    let source = "class A\n{\n    void M()\n    {\n        int c;\n        {\n            int d;\n        }\n        int e;\n    }\n}\n";
    check(source, &[], source);
}

#[test]
fn a_leading_comment_belongs_to_the_member_below_it() {
    let source = "class A\n{\n    int h;\n    // c\n    int i;\n}\n";
    let expected = "class A\n{\n    int h;\n\n    // c\n    int i;\n}\n";
    check(source, &[], expected);
}

#[test]
fn a_bare_block_statement_gets_no_blank_after_its_brace() {
    let source = "class A\n{\n    void M()\n    {\n        if (a > 0)\n        {\n            a = 1;\n        }\n        int z;\n    }\n}\n";
    let expected = "class A\n{\n    void M()\n    {\n        if (a > 0)\n        {\n            a = 1;\n        }\n\n        int z;\n    }\n}\n";
    check(source, &[], expected);
}

#[test]
fn a_trailing_comment_gets_no_separating_blank() {
    let source = "class A\n{\n    void M(int a)\n    {\n        switch (a)\n        {\n            case 1:\n                break;\n        }\n        // tail\n    }\n}\n";
    check(source, &[], source);
    let with_code = "class A\n{\n    void M(int a)\n    {\n        switch (a)\n        {\n            case 1:\n                break;\n        }\n        // c\n        M(a);\n    }\n}\n";
    let expected = "class A\n{\n    void M(int a)\n    {\n        switch (a)\n        {\n            case 1:\n                break;\n        }\n\n        // c\n        M(a);\n    }\n}\n";
    check(with_code, &[], expected);
}
