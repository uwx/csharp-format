//! Granular ReSharper `indent_*` / `outdent_*` behaviour, verified against the ReSharper
//! CLI (`Built-in: Reformat Code`, ReSharper 2026.2).

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
fn indent_inside_namespace() {
    let source = "namespace N\n{\n    class C\n    {\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["indent_inside_namespace = false"],
        "namespace N\n{\nclass C\n{\n}\n}\n",
    );
}

#[test]
fn preprocessor_indent_styles() {
    let source = "class C\n{\n#if DEBUG\n    int x;\n#endif\n#region R\n    int y;\n#endregion\n#pragma warning disable CS0169\n    int w;\n}\n";
    // Defaults: `#if`/`#pragma` at column zero, `#region` indented with the code, and the
    // region body padded with one blank line on each side.
    check(
        source,
        &[],
        "class C\n{\n#if DEBUG\n    int x;\n#endif\n\n    #region R\n\n    int y;\n\n    #endregion\n\n#pragma warning disable CS0169\n    int w;\n}\n",
    );
    check(
        source,
        &[
            "indent_preprocessor_if = usual_indent",
            "indent_preprocessor_other = usual_indent",
        ],
        "class C\n{\n    #if DEBUG\n    int x;\n    #endif\n\n    #region R\n\n    int y;\n\n    #endregion\n\n    #pragma warning disable CS0169\n    int w;\n}\n",
    );
    check(
        source,
        &["indent_preprocessor_region = outdent"],
        "class C\n{\n#if DEBUG\n    int x;\n#endif\n\n#region R\n\n    int y;\n\n#endregion\n\n#pragma warning disable CS0169\n    int w;\n}\n",
    );
}

#[test]
fn indent_type_constraints() {
    let source = "class D<T>\n    where T : class\n{\n}\n";
    check(source, &[], source);
    check(
        source,
        &["indent_type_constraints = false"],
        "class D<T>\nwhere T : class\n{\n}\n",
    );
}

#[test]
fn outdent_statement_labels() {
    let source = "class C\n{\n    void M()\n    {\n        start:\n        goto start;\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["outdent_statement_labels = true"],
        "class C\n{\n    void M()\n    {\n    start:\n        goto start;\n    }\n}\n",
    );
}

#[test]
fn nested_loop_statements_are_not_indented_by_default() {
    let source = "class C\n{\n    void M()\n    {\n        for (int i = 0; i < 1; i++)\n            for (int j = 0; j < 1; j++)\n                while (true)\n                    M();\n    }\n}\n";
    let expected = "class C\n{\n    void M()\n    {\n        for (int i = 0; i < 1; i++)\n        for (int j = 0; j < 1; j++)\n            while (true)\n                M();\n    }\n}\n";
    check(source, &[], expected);
    check(
        source,
        &["indent_nested_for_stmt = true"],
        "class C\n{\n    void M()\n    {\n        for (int i = 0; i < 1; i++)\n            for (int j = 0; j < 1; j++)\n                while (true)\n                    M();\n    }\n}\n",
    );
}

#[test]
fn nested_statements_of_other_kinds_stay_indented() {
    let source = "class C\n{\n    void M()\n    {\n        if (a)\n            if (b)\n                M();\n    }\n}\n";
    check(source, &[], source);
}

#[test]
fn raw_literal_string_indent_styles() {
    let source =
        "class C\n{\n    void M()\n    {\n        var s = \"\"\"\ntext\n\"\"\";\n    }\n}\n";
    check(
        source,
        &[],
        "class C\n{\n    void M()\n    {\n        var s = \"\"\"\n                text\n                \"\"\";\n    }\n}\n",
    );
    check(
        source,
        &["indent_raw_literal_string = indent"],
        "class C\n{\n    void M()\n    {\n        var s = \"\"\"\n            text\n            \"\"\";\n    }\n}\n",
    );
    check(
        source,
        &["indent_raw_literal_string = do_not_change"],
        source,
    );
}

#[test]
fn linq_clauses_indent_from_the_statement() {
    let source = "class C\n{\n    void M()\n    {\n        var q = from x in xs\n                where x > 0\n                select x;\n    }\n}\n";
    let expected = "class C\n{\n    void M()\n    {\n        var q = from x in xs\n            where x > 0\n            select x;\n    }\n}\n";
    check(source, &[], expected);
}

#[test]
fn continuous_indent_multiplier_scales_continuations() {
    let source =
        "class C\n{\n    void M()\n    {\n        Foo(Bar(Baz(1,\n            2)));\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["continuous_indent_multiplier = 2"],
        "class C\n{\n    void M()\n    {\n        Foo(Bar(Baz(1,\n                2)));\n    }\n}\n",
    );
}
