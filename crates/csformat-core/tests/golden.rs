//! Golden tests against ReSharper CLI ("Built-in: Reformat Code") output.
//!
//! `fixtures/*.cs` are the inputs; `fixtures/expected/*.cs` were produced by
//! `scripts/jbcmp.sh` running `dotnet jb cleanupcode`. Regenerate them with that
//! script whenever a fixture changes.

use std::fs;
use std::path::{Path, PathBuf};

use csformat_core::{FormatOptions, format_source, verify_source};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(fixtures_dir())
        .expect("fixtures directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.ends_with(".cs").then_some(name)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn matches_resharper_output() {
    let dir = fixtures_dir();
    let names = fixture_names();
    assert!(!names.is_empty(), "no fixtures found in {}", dir.display());

    let mut failures = Vec::new();
    for name in names {
        let source = fs::read_to_string(dir.join(&name)).expect("read fixture");
        let expected =
            fs::read_to_string(dir.join("expected").join(&name)).expect("read expected output");
        let output = format_source(&source, &FormatOptions::default()).output;
        if output != expected {
            failures.push(name);
        }
    }
    assert!(
        failures.is_empty(),
        "fixtures differ from ReSharper output: {failures:?}"
    );
}

#[test]
fn fixtures_are_lossless_and_idempotent() {
    let dir = fixtures_dir();
    let mut failures = Vec::new();
    for name in fixture_names() {
        let source = fs::read_to_string(dir.join(&name)).expect("read fixture");
        let report = verify_source(&source, &FormatOptions::default());
        if !report.ok {
            failures.push(format!("{name}: {}", report.summary()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
