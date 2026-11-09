//! Corpus verification harness: formats every C# file under the given paths and checks
//! that the transformation is lossless, idempotent, and does not add parse errors.

use anyhow::{Context, Result, bail};
use clap::Parser;
use csformat_core::config::{ConfigResolver, ResolvedConfig};
use csformat_core::verify_source;
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "csformat-verify",
    version,
    about = "Verify the formatter against a corpus of C# files"
)]
struct Cli {
    /// Files or directories to verify (defaults to the fetched corpora).
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Ignore .editorconfig/.DotSettings and use built-in defaults.
    #[arg(long)]
    no_config: bool,

    /// Parallel worker threads (0 selects the number of CPUs).
    #[arg(long, short = 'j', default_value_t = 0)]
    jobs: usize,

    /// Maximum number of failing files to report.
    #[arg(long, default_value_t = 25)]
    max_report: usize,

    /// Also treat files with existing lex/parse errors as failures.
    #[arg(long)]
    strict: bool,

    /// Emit a JSON summary instead of a human-readable one.
    #[arg(long)]
    json: bool,

    /// Also verify files inside bin/ and obj/ build output directories.
    #[arg(long)]
    include_build_output: bool,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            2
        }
    };
    std::process::exit(code);
}

#[derive(Serialize)]
struct Failure {
    path: String,
    lossless: bool,
    idempotent: bool,
    parse_error_regression: bool,
    input_lex_errors: usize,
    output_lex_errors: usize,
    input_parse_errors: usize,
    output_parse_errors: usize,
    summary: String,
}

impl Failure {
    fn reason(&self) -> String {
        let mut reasons = Vec::new();
        if !self.lossless {
            reasons.push("tokens changed");
        }
        if !self.idempotent {
            reasons.push("not idempotent");
        }
        if self.parse_error_regression {
            reasons.push("new parse errors");
        }
        if self.output_lex_errors > self.input_lex_errors {
            reasons.push("new lex errors");
        }
        if reasons.is_empty() {
            reasons.push("existing errors");
        }
        reasons.join(", ")
    }
}

#[derive(Default, Serialize)]
struct Summary {
    files: usize,
    verified: usize,
    skipped_non_utf8: usize,
    skipped_not_csharp: usize,
    ok: usize,
    failed: usize,
    lossless_failures: usize,
    idempotency_failures: usize,
    parse_regressions: usize,
    lex_regressions: usize,
    elapsed_ms: u128,
    failures: Vec<Failure>,
}

fn run(mut cli: Cli) -> Result<i32> {
    if cli.paths.is_empty() {
        cli.paths = default_paths();
    }
    let (files, skipped_not_csharp) = collect_files(&cli.paths, cli.include_build_output)?;
    if files.is_empty() {
        bail!("no C# files found in the given paths");
    }

    let resolver = Arc::new(ConfigResolver::new());
    let started = Instant::now();
    let results: Vec<Outcome> = run_parallel(cli.jobs, &files, |path| {
        verify_file(path, &resolver, cli.no_config)
    });
    let elapsed_ms = started.elapsed().as_millis();

    let mut summary = Summary {
        files: files.len(),
        skipped_not_csharp,
        elapsed_ms,
        ..Summary::default()
    };
    for outcome in results {
        match outcome {
            Outcome::Skipped => summary.skipped_non_utf8 += 1,
            Outcome::Failed(failure) => {
                summary.verified += 1;
                summary.failed += 1;
                if !failure.lossless {
                    summary.lossless_failures += 1;
                }
                if !failure.idempotent {
                    summary.idempotency_failures += 1;
                }
                if failure.parse_error_regression {
                    summary.parse_regressions += 1;
                }
                if failure.output_lex_errors > failure.input_lex_errors {
                    summary.lex_regressions += 1;
                }
                if summary.failures.len() < cli.max_report {
                    summary.failures.push(failure);
                }
            }
            Outcome::Ok(existing_errors) => {
                summary.verified += 1;
                if existing_errors && cli.strict {
                    summary.failed += 1;
                } else {
                    summary.ok += 1;
                }
            }
        }
    }

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).context("failed to serialize summary")?
        );
    } else {
        print_summary(&summary, cli.max_report);
    }

    if summary.failed > 0 { Ok(1) } else { Ok(0) }
}

enum Outcome {
    Skipped,
    Ok(bool),
    Failed(Failure),
}

fn verify_file(path: &Path, resolver: &ConfigResolver, no_config: bool) -> Outcome {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Outcome::Failed(Failure {
                path: path.display().to_string(),
                lossless: true,
                idempotent: true,
                parse_error_regression: false,
                input_lex_errors: 0,
                output_lex_errors: 0,
                input_parse_errors: 0,
                output_parse_errors: 0,
                summary: format!("read error: {err}"),
            });
        }
    };
    let Ok(source) = String::from_utf8(bytes) else {
        return Outcome::Skipped;
    };

    let resolved = if no_config {
        ResolvedConfig::with_defaults()
    } else {
        resolver.resolve(path)
    };
    let report = verify_source(&source, &resolved.options);
    if report.ok {
        Outcome::Ok(report.input_lex_errors > 0 || report.input_parse_errors > 0)
    } else {
        Outcome::Failed(Failure {
            path: path.display().to_string(),
            lossless: report.lossless,
            idempotent: report.idempotent,
            parse_error_regression: report.parse_error_regression,
            input_lex_errors: report.input_lex_errors,
            output_lex_errors: report.output_lex_errors,
            input_parse_errors: report.input_parse_errors,
            output_parse_errors: report.output_parse_errors,
            summary: report.summary(),
        })
    }
}

fn print_summary(summary: &Summary, max_report: usize) {
    println!(
        "verified {} file(s) in {} ms",
        summary.verified, summary.elapsed_ms
    );
    if summary.skipped_non_utf8 > 0 {
        println!("skipped {} non-UTF-8 file(s)", summary.skipped_non_utf8);
    }
    if summary.skipped_not_csharp > 0 {
        println!(
            "skipped {} non-C# fixture(s) (xml_ignore/xml_strict XML formatting tests)",
            summary.skipped_not_csharp
        );
    }
    println!("ok: {}", summary.ok);
    println!("failed: {}", summary.failed);
    if summary.failed > 0 {
        println!("  token/lossless: {}", summary.lossless_failures);
        println!("  idempotency: {}", summary.idempotency_failures);
        println!("  new parse errors: {}", summary.parse_regressions);
        println!("  new lex errors: {}", summary.lex_regressions);
        let shown = summary.failures.len().min(max_report);
        for failure in summary.failures.iter().take(shown) {
            println!(
                "  {}: {} ({})",
                failure.path,
                failure.reason(),
                failure.summary
            );
        }
        if summary.failed > shown {
            println!(
                "  ... {} more failure(s) (use --max-report to see more)",
                summary.failed - shown
            );
        }
    }
}

fn run_parallel<F>(jobs: usize, files: &[PathBuf], f: F) -> Vec<Outcome>
where
    F: Fn(&Path) -> Outcome + Sync,
{
    let collect = || -> Vec<Outcome> { files.par_iter().map(|path| f(path)).collect() };
    if jobs == 0 {
        return collect();
    }
    match rayon::ThreadPoolBuilder::new().num_threads(jobs).build() {
        Ok(pool) => pool.install(collect),
        Err(err) => {
            eprintln!("warning: could not create {jobs} worker threads ({err}); using default");
            collect()
        }
    }
}

fn collect_files(paths: &[PathBuf], include_build_output: bool) -> Result<(Vec<PathBuf>, usize)> {
    let mut files = Vec::new();
    // CSharpier also ships XML formatting fixtures under these folders; they are not C#.
    let mut skipped_not_csharp = 0usize;
    for path in paths {
        if path.is_file() {
            if !include_build_output && is_build_output(path) {
                continue;
            }
            files.push(path.clone());
            continue;
        }
        if !path.exists() {
            bail!("path does not exist: {}", path.display());
        }
        let mut builder = WalkBuilder::new(path);
        builder.standard_filters(true);
        if !include_build_output {
            builder.filter_entry(|entry| {
                if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    let name = entry.file_name().to_string_lossy();
                    !(name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("obj"))
                } else {
                    true
                }
            });
        }
        for entry in builder.build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    eprintln!("warning: {err}");
                    continue;
                }
            };
            let is_source = entry.path().extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cs")
                    || ext.eq_ignore_ascii_case("test")
                    || ext.eq_ignore_ascii_case("sample")
            });
            let xml_fixture = entry.path().components().any(|component| {
                matches!(
                    component.as_os_str().to_str(),
                    Some("xml_ignore" | "xml_strict")
                )
            });
            if is_source && xml_fixture {
                skipped_not_csharp += 1;
                continue;
            }
            if entry.file_type().is_some_and(|kind| kind.is_file()) && is_source {
                files.push(entry.into_path());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok((files, skipped_not_csharp))
}

fn is_build_output(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("obj")
    })
}

fn default_paths() -> Vec<PathBuf> {
    let candidates = ["corpus/roslyn", "corpus/csharpier"];
    let paths: Vec<PathBuf> = candidates
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths
    }
}
