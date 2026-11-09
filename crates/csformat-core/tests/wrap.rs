//! Wrapping-style behaviour, verified against the ReSharper CLI
//! (`Built-in: Reformat Code`, ReSharper 2026.2).

use std::path::Path;

use csformat_core::config::{WrapStyle, apply_key};
use csformat_core::{FormatOptions, format_source};

fn options() -> FormatOptions {
    let mut options = FormatOptions::default();
    options.wrap.max_length = Some(120);
    options.insert_final_newline = true;
    options
}

fn format(source: &str, options: &FormatOptions) -> String {
    format_source(source, options).output
}

const CALL: &str = "class C
{
    void M()
    {
        CallMethod(argumentNumberOne, argumentNumberTwo, argumentNumberThree, argumentNumberFour, argumentNumberFive, argumentNumberSix);
    }
}
";

#[test]
fn chop_if_long_puts_each_argument_on_its_own_line() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::ChopIfLong;
    let expected = "class C
{
    void M()
    {
        CallMethod(argumentNumberOne,
            argumentNumberTwo,
            argumentNumberThree,
            argumentNumberFour,
            argumentNumberFive,
            argumentNumberSix);
    }
}
";
    assert_eq!(format(CALL, &options), expected);
}

#[test]
fn chop_always_chops_even_short_calls() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::ChopAlways;
    let source = "class C
{
    void M()
    {
        CallMethod(a, b);
    }
}
";
    let expected = "class C
{
    void M()
    {
        CallMethod(a,
            b);
    }
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn wrap_if_long_fills_lines() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::WrapIfLong;
    let expected = "class C
{
    void M()
    {
        CallMethod(argumentNumberOne, argumentNumberTwo, argumentNumberThree, argumentNumberFour, argumentNumberFive,
            argumentNumberSix);
    }
}
";
    assert_eq!(format(CALL, &options), expected);
}

#[test]
fn lpar_and_rpar_preferences_apply_to_chopped_calls() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::ChopIfLong;
    options.wrap.after_invocation_lpar = true;
    options.wrap.before_invocation_rpar = true;
    let expected = "class C
{
    void M()
    {
        CallMethod(
            argumentNumberOne,
            argumentNumberTwo,
            argumentNumberThree,
            argumentNumberFour,
            argumentNumberFive,
            argumentNumberSix
        );
    }
}
";
    assert_eq!(format(CALL, &options), expected);
}

#[test]
fn wrap_before_comma_moves_separators_to_the_line_start() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::ChopIfLong;
    options.wrap.before_comma = true;
    let expected = "class C
{
    void M()
    {
        CallMethod(argumentNumberOne
            , argumentNumberTwo
            , argumentNumberThree
            , argumentNumberFour
            , argumentNumberFive
            , argumentNumberSix);
    }
}
";
    assert_eq!(format(CALL, &options), expected);
}

#[test]
fn max_arguments_on_line_breaks_after_the_limit() {
    let mut options = options();
    options.wrap.max_arguments_on_line = Some(2);
    let source = "class C
{
    void M()
    {
        CallMethod(a, b, c, d, e);
    }
}
";
    let expected = "class C
{
    void M()
    {
        CallMethod(a, b,
            c, d,
            e);
    }
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn parameter_style_chops_declarations() {
    let mut options = options();
    options.wrap.parameters = WrapStyle::ChopAlways;
    let source = "class C
{
    void M(string x, C obj)
    {
    }
}
";
    let expected = "class C
{
    void M(string x,
        C obj)
    {
    }
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn enum_members_chop_always() {
    let mut options = options();
    options.wrap.enum_declaration = WrapStyle::ChopAlways;
    let source = "enum E { A, B, C }
";
    let expected = "enum E
{
    A,
    B,
    C
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn simple_list_patterns_stay_inline_even_with_chop_always() {
    let mut options = options();
    options.wrap.list_pattern = WrapStyle::ChopAlways;
    let source = "class C
{
    bool M(int[] arr) => arr is [1, 2, 3];
}
";
    let expected = "class C
{
    bool M(int[] arr) => arr is [1, 2, 3];
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn wrap_style_keys_are_resolved() {
    let mut options = FormatOptions::default();
    let mut applied = Vec::new();
    apply_key(
        &mut options,
        "resharper_csharp_wrap_arguments_style",
        "chop_if_long",
        Path::new(".editorconfig"),
        &mut applied,
    );
    apply_key(
        &mut options,
        "csharp_wrap_parameters_style",
        "chop_always",
        Path::new(".editorconfig"),
        &mut applied,
    );
    apply_key(
        &mut options,
        "resharper_csharp_max_invocation_arguments_on_line",
        "2",
        Path::new(".editorconfig"),
        &mut applied,
    );
    assert_eq!(options.wrap.arguments, WrapStyle::ChopIfLong);
    assert_eq!(options.wrap.parameters, WrapStyle::ChopAlways);
    assert_eq!(options.wrap.max_arguments_on_line, Some(2));
    assert!(options.warnings.is_empty(), "{:?}", options.warnings);
}

#[test]
fn granular_space_overrides_match_resharper() {
    // Verified with the ReSharper CLI using the same .editorconfig keys.
    let mut options = FormatOptions::default();
    options.insert_final_newline = true;
    let mut applied = Vec::new();
    for (key, value) in [
        (
            "resharper_csharp_space_before_method_call_parentheses",
            "true",
        ),
        ("resharper_csharp_space_within_parentheses", "true"),
        ("resharper_csharp_space_within_if_parentheses", "true"),
        (
            "resharper_csharp_space_before_array_access_brackets",
            "true",
        ),
        ("resharper_csharp_space_before_method_parentheses", "true"),
    ] {
        apply_key(
            &mut options,
            key,
            value,
            Path::new(".editorconfig"),
            &mut applied,
        );
    }
    let source = "class C
{
    int M(int a)
    {
        if (a > 0)
        {
            return M(a) + a[0];
        }
        var z = (a + a) * 2;
        return z;
    }

    int M2(int b) => b;
}
";
    let expected = "class C
{
    int M (int a)
    {
        if ( a > 0 )
        {
            return M (a) + a [0];
        }

        var z = ( a + a ) * 2;
        return z;
    }

    int M2 (int b) => b;
}
";
    assert_eq!(format(source, &options), expected);
}
