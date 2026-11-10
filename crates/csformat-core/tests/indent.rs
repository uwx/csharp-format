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

#[test]
fn binary_chains_align_at_the_expression_start() {
    let source = "class C\n{\n    int M()\n    {\n        var v = 1\n            + 2\n            + 3;\n    }\n}\n";
    let expected = "class C\n{\n    int M()\n    {\n        var v = 1\n                + 2\n                + 3;\n    }\n}\n";
    check(source, &[], expected);
    check(
        source,
        &["align_multiline_binary_expressions_chain = false"],
        "class C\n{\n    int M()\n    {\n        var v = 1\n            + 2\n            + 3;\n    }\n}\n",
    );
    check(
        source,
        &["outdent_binary_ops = true"],
        "class C\n{\n    int M()\n    {\n        var v = 1\n              + 2\n              + 3;\n    }\n}\n",
    );
}

#[test]
fn call_chains_align_when_asked() {
    let source = "class C\n{\n    int M(int a)\n    {\n        var w = a\n            .ToString()\n            .Length;\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["align_multiline_calls_chain = true"],
        "class C\n{\n    int M(int a)\n    {\n        var w = a\n                .ToString()\n                .Length;\n    }\n}\n",
    );
    check(
        source,
        &["outdent_dots = true"],
        "class C\n{\n    int M(int a)\n    {\n        var w = a\n           .ToString()\n           .Length;\n    }\n}\n",
    );
}

#[test]
fn broken_for_headers_align_under_the_first_token() {
    let source = "class C
{
    void M()
    {
        for (var i = 0;
            i < 1;
            i++)
        {
        }
    }
}
";
    let expected = "class C
{
    void M()
    {
        for (var i = 0;
             i < 1;
             i++)
        {
        }
    }
}
";
    check(source, &[], expected);
}

#[test]
fn aligned_base_lists_line_up_under_the_first_entry() {
    let source = "class D
    : A,
        B
{
}
";
    check(source, &[], source);
    check(
        source,
        &["align_multiline_extends_list = true"],
        "class D
    : A,
      B
{
}
",
    );
}

#[test]
fn tabular_alignment_of_fields_and_variables() {
    let source = "class C\n{\n    int a = 1;\n    int bb = 2;\n    string ccc = \"x\";\n\n    void M()\n    {\n        int x = 1;\n        int yyyy = 2;\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["int_align_fields = true"],
        "class C\n{\n    int    a   = 1;\n    int    bb  = 2;\n    string ccc = \"x\";\n\n    void M()\n    {\n        int x = 1;\n        int yyyy = 2;\n    }\n}\n",
    );
    check(
        source,
        &["int_align_variables = true"],
        "class C\n{\n    int a = 1;\n    int bb = 2;\n    string ccc = \"x\";\n\n    void M()\n    {\n        int x    = 1;\n        int yyyy = 2;\n    }\n}\n",
    );
}

#[test]
fn tabular_alignment_of_assignments_and_binary_expressions() {
    let source = "class C\n{\n    void M()\n    {\n        a = 3;\n        bb = 4;\n        var v = 1 + 2;\n        var wwww = 3 + 4;\n    }\n}\n";
    check(source, &[], source);
    check(
        source,
        &["int_align_assignments = true"],
        "class C\n{\n    void M()\n    {\n        a  = 3;\n        bb = 4;\n        var v = 1 + 2;\n        var wwww = 3 + 4;\n    }\n}\n",
    );
    check(
        source,
        &["int_align_binary_expressions = true"],
        "class C\n{\n    void M()\n    {\n        a = 3;\n        bb = 4;\n        var v = 1    + 2;\n        var wwww = 3 + 4;\n    }\n}\n",
    );
}

#[test]
fn tabular_alignment_of_properties() {
    let source = "class C\n{\n    int P1 { get; set; }\n    int PPPP { get; set; }\n}\n";
    check(
        source,
        &["int_align_properties = true"],
        "class C\n{\n    int P1   { get; set; }\n    int PPPP { get; set; }\n}\n",
    );
}

#[test]
fn per_category_brace_placement() {
    let source =
        "namespace N\n{\n    class C\n    {\n        void M()\n        {\n        }\n    }\n}\n";
    check(
        source,
        &["invocable_declaration_braces = end_of_line"],
        "namespace N\n{\n    class C\n    {\n        void M() {\n        }\n    }\n}\n",
    );
    check(
        source,
        &["type_declaration_braces = end_of_line"],
        "namespace N {\n    class C {\n        void M()\n        {\n        }\n    }\n}\n",
    );
    check(
        source,
        &[
            "type_declaration_braces = end_of_line",
            "invocable_declaration_braces = next_line",
        ],
        "namespace N {\n    class C {\n        void M()\n        {\n        }\n    }\n}\n",
    );
}
