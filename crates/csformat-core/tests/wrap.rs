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

const BASE_LIST: &str = "namespace P
{
    public class Derived : Base, IFoo, IBar
    {
    }
}
";

#[test]
fn a_type_name_moves_down_when_its_header_does_not_fit() {
    let mut options = options();
    options.wrap.max_length = Some(30);
    let expected = "namespace P
{
    public class
        Derived : Base, IFoo,
        IBar
    {
    }
}
";
    assert_eq!(format(BASE_LIST, &options), expected);
}

#[test]
fn a_type_name_stays_up_when_its_first_base_entry_fits_nowhere() {
    let source = "namespace P
{
    public class DerivedLong : SomeVeryVeryLongBaseClassName123456789012345
    {
    }
}
";
    let mut options = options();
    options.wrap.max_length = Some(40);
    let expected = "namespace P
{
    public class DerivedLong :
        SomeVeryVeryLongBaseClassName123456789012345
    {
    }
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn a_long_type_name_breaks_after_the_keyword() {
    let source = "namespace P
{
    public class SomeVeryLongClassName1234
    {
    }
}
";
    let mut options = options();
    options.wrap.max_length = Some(40);
    let expected = "namespace P
{
    public class
        SomeVeryLongClassName1234
    {
    }
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn chop_always_breaks_after_the_colon_before_moving_the_type_name() {
    let mut options = options();
    options.wrap.max_length = Some(30);
    options.wrap.extends_list = WrapStyle::ChopAlways;
    let expected = "namespace P
{
    public class Derived :
        Base,
        IFoo,
        IBar
    {
    }
}
";
    assert_eq!(format(BASE_LIST, &options), expected);
}

#[test]
fn chop_if_long_chops_a_base_list_that_does_not_fit() {
    let mut options = options();
    options.wrap.max_length = Some(40);
    options.wrap.extends_list = WrapStyle::ChopIfLong;
    let expected = "namespace P
{
    public class Derived : Base,
        IFoo,
        IBar
    {
    }
}
";
    assert_eq!(format(BASE_LIST, &options), expected);
}

#[test]
fn chop_if_long_chops_the_entries_of_a_moved_base_colon() {
    let source = "namespace P
{
    public class Prim(int a) : Base(first), IFoo, IBar
    {
    }
}
";
    let mut options = options();
    options.wrap.max_length = Some(30);
    options.wrap.extends_list = WrapStyle::ChopIfLong;
    let expected = "namespace P
{
    public class Prim(int a)
        : Base(first),
            IFoo,
            IBar
    {
    }
}
";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn force_chop_compound_conditions_break_every_operand() {
    let source = "class C\n{\n    void M(int a)\n    {\n        if (a > 0 && a < 10 && a != 5)\n        {\n            M(a);\n        }\n    }\n}\n";
    let mut options = options();
    assert_eq!(format(source, &options), source);
    options.wrap.force_chop_compound_if = true;
    let expected = "class C\n{\n    void M(int a)\n    {\n        if (a > 0 &&\n            a < 10 &&\n            a != 5)\n        {\n            M(a);\n        }\n    }\n}\n";
    assert_eq!(format(source, &options), expected);
}

#[test]
fn compound_chop_is_per_statement_and_aligns_with_the_condition() {
    let source = "class C
{
    void M(int a)
    {
        while (a > 0 && a < 10)
        {
            a--;
        }
        do
        {
            a++;
        }
        while (a > 0 && a < 10);
    }
}
";
    let mut while_options = options();
    while_options.wrap.force_chop_compound_while = true;
    let while_chopped = "class C
{
    void M(int a)
    {
        while (a > 0 &&
               a < 10)
        {
            a--;
        }

        do
        {
            a++;
        } while (a > 0 && a < 10);
    }
}
";
    assert_eq!(format(source, &while_options), while_chopped);

    let mut do_options = options();
    do_options.wrap.force_chop_compound_do = true;
    let do_chopped = "class C
{
    void M(int a)
    {
        while (a > 0 && a < 10)
        {
            a--;
        }

        do
        {
            a++;
        } while (a > 0 &&
                 a < 10);
    }
}
";
    assert_eq!(format(source, &do_options), do_chopped);
}

// A chopped parameter list breaks at its own commas only: the commas of a generic argument
// list inside a parameter are not separators (probe: `UI/Components/SettingsView.cs`).
#[test]
fn chopped_parameters_do_not_break_inside_generic_arguments() {
    let mut options = options();
    options.wrap.parameters = WrapStyle::ChopIfLong;
    let source = "class C
{
    private sealed class InputView(KeyboardNavigation navigation, Func<Key, bool> handleKey, Func<bool> capturing, Func<bool> previewActive, Func<UiState.RaceState?> menuHudPreview, Func<bool> hudPreviewActive) : View
    {
    }
}
";
    let expected = "class C
{
    private sealed class InputView(
        KeyboardNavigation navigation,
        Func<Key, bool> handleKey,
        Func<bool> capturing,
        Func<bool> previewActive,
        Func<UiState.RaceState?> menuHudPreview,
        Func<bool> hudPreviewActive) : View
    {
    }
}
";
    assert_eq!(format(source, &options), expected);
}

// A single lambda argument that spans lines does not make the enclosing call "chopped".
#[test]
fn a_lambda_body_break_does_not_chop_the_argument_list() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::ChopIfLong;
    options.wrap.max_length = Some(130);
    let source = "class C
{
    void M()
    {
        var inside = hit.Any(target =>
            target.Id == 1 && target.Name == \"x\");
    }
}
";
    let expected = "class C
{
    void M()
    {
        var inside = hit.Any(target =>
            target.Id == 1 && target.Name == \"x\");
    }
}
";
    assert_eq!(format(source, &options), expected);
}

// With a single argument there is no comma to chop at, so a long call is left long
// (probe: `new ColorRamp(colors.Select(...).ToArray())` in NFM-World).
#[test]
fn a_long_call_with_one_argument_is_not_chopped() {
    let mut options = options();
    options.wrap.arguments = WrapStyle::ChopIfLong;
    options.wrap.max_length = Some(130);
    let source = "class C
{
    void M()
    {
        var ramp = new ColorRamp(colors.Select((c, i) => (colorPos?[i] ?? (float)i / (colors.Length - 1), c)).ToArray());
    }
}
";
    let expected = "class C
{
    void M()
    {
        var ramp = new ColorRamp(colors.Select((c, i) => (colorPos?[i] ?? (float)i / (colors.Length - 1), c)).ToArray());
    }
}
";
    assert_eq!(format(source, &options), expected);
}

// An initializer that does not fit is filled, and the elements stay at the brace indent.
#[test]
fn a_long_initializer_fills_and_keeps_the_element_indent() {
    let mut options = options();
    options.wrap.max_length = Some(130);
    let mut elements = Vec::new();
    for n in 1..=40 {
        elements.push(format!("\"e{n}\""));
    }
    let source = format!(
        "class C
{{
    void M()
    {{
        var names = new HashSet<string>(StringComparer.Ordinal)
        {{
            {}
        }};
    }}
}}
",
        elements.join(", ")
    );
    // ReSharper fills 18 / 16 / 6 elements per line at 130 columns.
    let line = |range: std::ops::Range<usize>| elements[range].join(", ");
    let expected = format!(
        "class C
{{
    void M()
    {{
        var names = new HashSet<string>(StringComparer.Ordinal)
        {{
            {},
            {},
            {}
        }};
    }}
}}
",
        line(0..18),
        line(18..34),
        line(34..40)
    );
    assert_eq!(format(&source, &options), expected);
}

// `max_initializer_elements_on_line` only applies while the initializer is re-flowed
// (`keep_existing_initializer_arrangement = false`); ReSharper keeps the author's grouping
// with the default `keep_existing_*` setting.
#[test]
fn the_initializer_element_limit_only_applies_when_reflowing() {
    let mut kept = options();
    kept.wrap.max_length = Some(130);
    kept.wrap.max_initializer_elements_on_line = Some(1);
    let mut reflowed = kept.clone();
    reflowed.keep.initializer_arrangement = false;
    let source = "class C
{
    void M()
    {
        var values = new Foo
        {
            A = 0, B = 1, C = 2, D = 3, E = 4, F = 5, A = 6, B = 7, C = 8, D = 9, E = 10, F = 11, A = 12, B = 13
        };
    }
}
";
    assert_eq!(format(source, &kept), source);
    let expected = "class C
{
    void M()
    {
        var values = new Foo
        {
            A = 0,
            B = 1,
            C = 2,
            D = 3,
            E = 4,
            F = 5,
            A = 6,
            B = 7,
            C = 8,
            D = 9,
            E = 10,
            F = 11,
            A = 12,
            B = 13
        };
    }
}
";
    assert_eq!(format(source, &reflowed), expected);
}

// A call chain in the true branch of a conditional indents past the `?` line.
#[test]
fn a_ternary_true_branch_chain_indents_under_the_question_mark() {
    let mut options = options();
    options.wrap.max_length = Some(130);
    options.wrap.chained_method_calls = true;
    let source = "class C
{
    void M()
    {
        console.RegisterArgumentAutocompleter(
            \"create\",
            (args, position) =>
                position == 0
                    ? BackendGameSparker.stage_parts.Select(part => part.FileName)
                        .Concat(BackendGameSparker.vendor_stage_parts.Select(part => part.FileName))
                        .ToArray()
                    : []
        );
    }
}
";
    assert_eq!(format(source, &options), source);
}
