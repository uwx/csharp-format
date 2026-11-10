//! Granular ReSharper `keep_existing_*` behaviour, verified against the ReSharper CLI
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

fn check(source: &str, keys: &[&str], expected: &str) {
    let output = format_source(source, &options(keys)).output;
    assert_eq!(output, expected.trim_end_matches('\n'), "keys: {keys:?}");
}

#[test]
fn attribute_sections_are_normalized_unless_kept() {
    let source = "[A] [B]\nclass C\n{\n}\n";
    check(source, &[], "[A]\n[B]\nclass C\n{\n}\n");
    check(
        source,
        &["keep_existing_attribute_arrangement = true"],
        source,
    );
}

#[test]
fn enum_arrangement_is_normalized_unless_kept() {
    let source = "enum E { A, B }\n";
    check(source, &[], "enum E\n{\n    A,\n    B\n}\n");
    check(source, &["keep_existing_enum_arrangement = true"], source);
}

#[test]
fn embedded_blocks_are_normalized_unless_kept() {
    let source = "class C\n{\n    void M(int a)\n    {\n        if (a > 0) { M(a); }\n    }\n}\n";
    check(
        source,
        &[],
        "class C\n{\n    void M(int a)\n    {\n        if (a > 0)\n        {\n            M(a);\n        }\n    }\n}\n",
    );
    check(
        source,
        &["keep_existing_embedded_block_arrangement = true"],
        source,
    );
}

#[test]
fn expression_member_arrow_joins_when_not_kept() {
    let source = "class C\n{\n    int M()\n        => 1;\n}\n";
    check(source, &[], source);
    check(
        source,
        &["keep_existing_expr_member_arrangement = false"],
        "class C\n{\n    int M() => 1;\n}\n",
    );
}

#[test]
fn multi_line_constructs_collapse_when_not_kept() {
    let source = "class C\n{\n    void M(int a)\n    {\n        var p = new Point\n        {\n            X = 1,\n        };\n        int[] c =\n        [\n            1,\n        ];\n        bool b = a is\n        {\n            X: 1,\n        };\n        M(\n            a);\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["keep_existing_initializer_arrangement = false"],
        "class C\n{\n    void M(int a)\n    {\n        var p = new Point { X = 1, };\n        int[] c =\n        [\n            1,\n        ];\n        bool b = a is\n        {\n            X: 1,\n        };\n        M(\n            a);\n    }\n}\n",
    );
    check(
        source,
        &["keep_existing_list_patterns_arrangement = false"],
        "class C\n{\n    void M(int a)\n    {\n        var p = new Point\n        {\n            X = 1,\n        };\n        int[] c = [1,];\n        bool b = a is\n        {\n            X: 1,\n        };\n        M(\n            a);\n    }\n}\n",
    );
    check(
        source,
        &["keep_existing_property_patterns_arrangement = false"],
        "class C\n{\n    void M(int a)\n    {\n        var p = new Point\n        {\n            X = 1,\n        };\n        int[] c =\n        [\n            1,\n        ];\n        bool b = a is { X: 1, };\n        M(\n            a);\n    }\n}\n",
    );
    check(
        source,
        &["keep_existing_invocation_parens_arrangement = false"],
        "class C\n{\n    void M(int a)\n    {\n        var p = new Point\n        {\n            X = 1,\n        };\n        int[] c =\n        [\n            1,\n        ];\n        bool b = a is\n        {\n            X: 1,\n        };\n        M(a);\n    }\n}\n",
    );
}

#[test]
fn declaration_parameter_lists_collapse_when_not_kept() {
    let source =
        "class C\n{\n    void M(\n        int a)\n    {\n    }\n}\n\nrecord R(\n    int X);\n";
    check(source, &[], source);
    check(
        source,
        &["keep_existing_declaration_parens_arrangement = false"],
        "class C\n{\n    void M(int a)\n    {\n    }\n}\n\nrecord R(int X);\n",
    );
    check(
        source,
        &["keep_existing_primary_constructor_declaration_parens_arrangement = false"],
        "class C\n{\n    void M(\n        int a)\n    {\n    }\n}\n\nrecord R(int X);\n",
    );
}

#[test]
fn record_parameter_attributes_move_down_when_asked() {
    let source = "record R([Obsolete] int X);\n";
    check(source, &[], source);
    check(
        source,
        &["place_record_field_attribute_on_same_line = never"],
        "record R(\n    [Obsolete]\n    int X);\n",
    );
}

#[test]
fn attribute_sections_follow_the_run_placement() {
    // Fields and single-line properties keep their sections on the declaration line; a run
    // that owns its line gets one section per line unless the arrangement is kept.
    let source = "class C\n{\n    [A] [B] int f;\n\n    [A] [B]\n    void M()\n    {\n    }\n\n    [A] [B]\n    int P { get; set; }\n}\n";
    check(
        source,
        &[],
        "class C\n{\n    [A] [B] int f;\n\n    [A]\n    [B]\n    void M()\n    {\n    }\n\n    [A] [B] int P { get; set; }\n}\n",
    );
    check(
        source,
        &["keep_existing_attribute_arrangement = true"],
        source,
    );
}
