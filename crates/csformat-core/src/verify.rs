//! Self-verification of the formatter.
//!
//! Verification is intentionally independent of the layout code: it re-lexes the formatted
//! output and checks the properties we care about:
//!
//! * **Lossless** — the sequence of token kinds and the token text is unchanged.
//! * **Idempotent** — formatting the output again produces the same output.
//! * **No new lexical errors** — formatting cannot introduce unterminated literals etc.
//! * **No new parse errors** — the tree-sitter error count does not grow.
//! * **No panics** — callers run this per file; the engine itself never indexes blindly.
//!
//! The report is cheap to build and is used both by unit tests and the corpus harness.

use crate::config::FormatOptions;
use crate::format::format_source;
use crate::lexer::lex;
use crate::syntax::Syntax;

/// Outcome of verifying one document.
#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub tokens: usize,
    pub changed: bool,
    pub lossless: bool,
    pub idempotent: bool,
    pub input_lex_errors: usize,
    pub output_lex_errors: usize,
    pub input_parse_errors: usize,
    pub output_parse_errors: usize,
    pub parse_error_regression: bool,
    pub ok: bool,
}

impl VerifyReport {
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} tokens, {} -> {} bytes, changed={}, lossless={}, idempotent={}, lex {}->{}, parse {}->{}",
            self.tokens,
            self.input_bytes,
            self.output_bytes,
            self.changed,
            self.lossless,
            self.idempotent,
            self.input_lex_errors,
            self.output_lex_errors,
            self.input_parse_errors,
            self.output_parse_errors
        )
    }
}

/// Compares token text across formatting.
///
/// The final emitted newline can be absorbed by a trailing *unterminated* block comment
/// or raw string, because those tokens extend to end of file. In that case the output
/// token legitimately contains one more line break than the input token.
pub fn token_text_matches(input: &str, output: &str, is_last: bool) -> bool {
    if input == output {
        return true;
    }
    // Raw strings may be re-indented as a whole; compare the resulting value.
    if let (Some(a), Some(b)) = (raw_string_value(input), raw_string_value(output)) {
        if a == b {
            return true;
        }
    }
    is_last
        && ((output.starts_with(input) && matches!(&output[input.len()..], "\n" | "\r\n"))
            || (input.starts_with(output) && matches!(&input[output.len()..], "\n" | "\r\n")))
}

/// Value of a multi-line raw string, with the closing delimiter's indentation removed.
fn raw_string_value(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut prefix = 0;
    while prefix < bytes.len() && bytes[prefix] == b'$' {
        prefix += 1;
    }
    if !text[prefix..].starts_with("\"\"\"") {
        return None;
    }
    let last_break = text.rfind('\n')?;
    let head = &text[..last_break];
    let tail = &text[last_break + 1..];
    let closing = tail.trim_start_matches([' ', '\t']);
    if !closing.starts_with("\"\"\"") || !closing.bytes().all(|b| b == b'"') {
        return None;
    }
    let old_indent = tail.chars().count() - closing.chars().count();
    let mut value = String::new();
    for (index, line) in head.split('\n').enumerate() {
        if index > 0 {
            value.push('\n');
        }
        let mut removed = 0;
        let mut rest = line;
        while removed < old_indent {
            match rest.as_bytes().first() {
                Some(b' ') => {
                    rest = &rest[1..];
                    removed += 1;
                }
                Some(b'\t') => {
                    rest = &rest[1..];
                    removed += 4;
                }
                _ => break,
            }
        }
        value.push_str(rest);
    }
    Some(value)
}

/// Format `source` and check the formatter's invariants.
#[must_use]
pub fn verify_source(source: &str, options: &FormatOptions) -> VerifyReport {
    let first = format_source(source, options);
    let input = lex(source);
    let output = lex(&first.output);

    let lossless = input.tokens.len() == output.tokens.len()
        && input
            .tokens
            .iter()
            .zip(&output.tokens)
            .enumerate()
            .all(|(i, (a, b))| {
                a.kind == b.kind
                    && token_text_matches(
                        a.text(source),
                        b.text(&first.output),
                        i + 1 == input.tokens.len(),
                    )
            });

    let second = format_source(&first.output, options);
    let idempotent = second.output == first.output;

    let input_parse_errors = Syntax::parse(source).error_count();
    let output_parse_errors = Syntax::parse(&first.output).error_count();
    let parse_error_regression = output_parse_errors > input_parse_errors;

    let ok = lossless
        && idempotent
        && output.errors.len() <= input.errors.len()
        && !parse_error_regression;

    VerifyReport {
        input_bytes: source.len(),
        output_bytes: first.output.len(),
        tokens: input.tokens.len(),
        changed: first.output != source,
        lossless,
        idempotent,
        input_lex_errors: input.errors.len(),
        output_lex_errors: output.errors.len(),
        input_parse_errors,
        output_parse_errors,
        parse_error_regression,
        ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_clean_source() {
        let report = verify_source(
            "class A{public void M(){int x=1;if(x>0){x++;}}}",
            &FormatOptions::default(),
        );
        assert!(report.ok, "{}", report.summary());
        assert!(report.lossless);
        assert!(report.idempotent);
        assert_eq!(report.parse_error_regression, false);
    }

    #[test]
    fn verifies_broken_source() {
        let report = verify_source(
            "class A { void M( { int x = ; /* unterminated",
            &FormatOptions::default(),
        );
        assert!(report.lossless, "{}", report.summary());
        assert!(report.idempotent, "{}", report.summary());
        assert!(!report.parse_error_regression, "{}", report.summary());
    }

    #[test]
    fn verifies_raw_strings_and_directives() {
        let source = "#if DEBUG\r\nclass A{void M(){var s=\"\"\"\r\n a\r\n \"\"\";}}\r\n#endif\r\n";
        let report = verify_source(source, &FormatOptions::default());
        assert!(report.ok, "{}", report.summary());
    }
}
