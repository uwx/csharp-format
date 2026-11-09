//! `csharp-format` — format C# files using Roslyn/ReSharper configuration.

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use csformat_core::config::{ConfigResolver, ResolvedConfig};
use csformat_core::format_source;
use ignore::WalkBuilder;
use rayon::prelude::*;
use similar::TextDiff;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "csharp-format",
    version,
    about = "Format C# source with Roslyn and ReSharper settings",
    long_about = "A lossless C# formatter that reads .editorconfig (csharp_* and resharper_*) and \
                  .DotSettings files. With no subcommand it formats the given paths in place."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Files or directories to format (defaults to the current directory).
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Format files in place.
    Format(FormatArgs),
    /// Check formatting without writing; exits with status 1 when files would change.
    Check(CheckArgs),
    /// Format standard input and write the result to standard output.
    Stdin(StdinArgs),
    /// Print the resolved formatting options for a path.
    Config(ConfigArgs),
    /// Explain which configuration files apply to a path and what they set.
    Explain(ExplainArgs),
}

#[derive(Args, Clone)]
struct CommonArgs {
    /// Ignore .editorconfig/.DotSettings and use built-in defaults.
    #[arg(long)]
    no_config: bool,
    /// Print configuration warnings and per-file diagnostics.
    #[arg(long, short = 'v')]
    verbose: bool,
    /// Also walk files inside bin/ and obj/ build output directories.
    #[arg(long)]
    include_build_output: bool,
}

#[derive(Args, Clone)]
struct FormatArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Write the formatted result to standard output instead of modifying files.
    #[arg(long)]
    stdout: bool,
    /// Print a unified diff instead of writing files.
    #[arg(long)]
    diff: bool,
    /// Parallel worker threads (0 selects the number of CPUs).
    #[arg(long, short = 'j', default_value_t = 0)]
    jobs: usize,
    /// Files or directories to format (defaults to the current directory).
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[derive(Args, Clone)]
struct CheckArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Only set the exit status; do not list files that would change.
    #[arg(long, short = 'q')]
    quiet: bool,
    /// Parallel worker threads (0 selects the number of CPUs).
    #[arg(long, short = 'j', default_value_t = 0)]
    jobs: usize,
    /// Files or directories to check (defaults to the current directory).
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[derive(Args, Clone)]
struct StdinArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Resolve configuration as if the input came from this file name.
    #[arg(long, value_name = "PATH")]
    file_name: Option<PathBuf>,
    /// Exit with status 1 when the input is not already formatted.
    #[arg(long)]
    check: bool,
    /// Print a unified diff instead of the formatted source.
    #[arg(long)]
    diff: bool,
}

#[derive(Args, Clone)]
struct ConfigArgs {
    /// File whose configuration should be resolved.
    #[arg(value_name = "PATH", default_value = ".")]
    path: PathBuf,
    /// Emit JSON instead of a human-readable summary.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone)]
struct ExplainArgs {
    /// File whose configuration should be explained.
    #[arg(value_name = "PATH", default_value = ".")]
    path: PathBuf,
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

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        None => {
            let paths = if cli.paths.is_empty() {
                vec![PathBuf::from(".")]
            } else {
                cli.paths
            };
            run_format(FormatArgs {
                common: CommonArgs {
                    no_config: false,
                    verbose: false,
                    include_build_output: false,
                },
                stdout: false,
                diff: false,
                jobs: 0,
                paths,
            })
        }
        Some(Command::Format(args)) => run_format(args),
        Some(Command::Check(args)) => run_check(args),
        Some(Command::Stdin(args)) => run_stdin(args),
        Some(Command::Config(args)) => run_config(args),
        Some(Command::Explain(args)) => run_explain(args),
    }
}

fn run_format(mut args: FormatArgs) -> Result<i32> {
    if args.stdout && args.diff {
        bail!("--stdout and --diff are mutually exclusive");
    }
    if args.paths.is_empty() {
        args.paths.push(PathBuf::from("."));
    }
    let files = collect_files(&args.paths, args.common.include_build_output)?;
    let resolver = Arc::new(ConfigResolver::new());

    let results = run_parallel(args.jobs, &files, |path| {
        process_file(path, &resolver, &args.common)
    });

    let mut changed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut collected = Vec::new();
    for result in results {
        match result {
            Ok(Some(file)) => {
                if file.changed {
                    changed += 1;
                }
                collected.push(file);
            }
            Ok(None) => skipped += 1,
            Err(err) => {
                failed += 1;
                eprintln!("error: {err:#}");
            }
        }
    }

    for file in &collected {
        if args.common.verbose {
            for warning in &file.warnings {
                eprintln!("{}: {warning}", file.path.display());
            }
        }
        if !file.changed {
            continue;
        }
        if args.stdout {
            print!("{}", file.output);
        } else if args.diff {
            print!("{}", unified_diff(&file.path, &file.input, &file.output));
        } else {
            std::fs::write(&file.path, &file.output)
                .with_context(|| format!("failed to write {}", file.path.display()))?;
            if args.common.verbose {
                eprintln!("formatted {}", file.path.display());
            }
        }
    }

    if !args.stdout && !args.diff && args.common.verbose {
        eprintln!(
            "{} file(s) changed, {} unchanged, {} skipped",
            changed,
            collected.len().saturating_sub(changed),
            skipped
        );
    }
    if failed > 0 { Ok(2) } else { Ok(0) }
}

fn run_check(mut args: CheckArgs) -> Result<i32> {
    if args.paths.is_empty() {
        args.paths.push(PathBuf::from("."));
    }
    let files = collect_files(&args.paths, args.common.include_build_output)?;
    let resolver = Arc::new(ConfigResolver::new());

    let results = run_parallel(args.jobs, &files, |path| {
        process_file(path, &resolver, &args.common)
    });

    let mut changed = Vec::new();
    let mut failed = 0usize;
    let mut skipped = 0usize;
    for result in results {
        match result {
            Ok(Some(file)) => {
                if file.changed {
                    if !args.quiet {
                        println!("{}", file.path.display());
                    }
                    changed.push(file);
                }
            }
            Ok(None) => skipped += 1,
            Err(err) => {
                failed += 1;
                eprintln!("error: {err:#}");
            }
        }
    }
    if !args.quiet && skipped > 0 {
        eprintln!("skipped {skipped} non-UTF-8 file(s)");
    }
    if !args.quiet && !changed.is_empty() {
        eprintln!("{} file(s) would be reformatted", changed.len());
    }
    if failed > 0 {
        Ok(2)
    } else if changed.is_empty() {
        Ok(0)
    } else {
        Ok(1)
    }
}

fn run_stdin(args: StdinArgs) -> Result<i32> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read standard input")?;

    let resolved = match &args.file_name {
        Some(path) => resolve(&ConfigResolver::new(), path, &args.common),
        None => ResolvedConfig::with_defaults(),
    };
    if args.common.verbose {
        for warning in &resolved.options.warnings {
            eprintln!("{warning}");
        }
    }

    let result = format_source(&input, &resolved.options);
    let changed = result.output != input;

    if args.diff {
        print!(
            "{}",
            unified_diff(Path::new("(stdin)"), &input, &result.output)
        );
        return Ok(if changed { 1 } else { 0 });
    }
    if args.check {
        if changed && !args.common.verbose {
            eprintln!("stdin is not formatted");
        }
        return Ok(if changed { 1 } else { 0 });
    }
    print!("{}", result.output);
    Ok(0)
}

fn run_config(args: ConfigArgs) -> Result<i32> {
    let path = args.path;
    let resolved = ConfigResolver::new().resolve(&path);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&resolved.options)
                .context("failed to serialize options")?
        );
    } else {
        print_summary(&resolved);
    }
    Ok(0)
}

fn run_explain(args: ExplainArgs) -> Result<i32> {
    let resolved = ConfigResolver::new().resolve(&args.path);
    println!("file: {}", args.path.display());
    if resolved.sources.is_empty() {
        println!("sources: none (built-in defaults)");
    } else {
        println!("sources:");
        for source in &resolved.sources {
            println!("  {}", source.display());
        }
    }
    if resolved.applied.is_empty() {
        println!("applied: none");
    } else {
        println!("applied:");
        for setting in &resolved.applied {
            println!(
                "  {} = {}  ({})",
                setting.key,
                setting.value,
                setting.source.display()
            );
        }
    }
    if !resolved.options.warnings.is_empty() {
        println!("warnings:");
        for warning in &resolved.options.warnings {
            println!("  {warning}");
        }
    }
    Ok(0)
}

fn print_summary(resolved: &ResolvedConfig) {
    let options = &resolved.options;
    println!("indent_style: {:?}", options.indent_style);
    println!("indent_size: {}", options.indent_size);
    println!("tab_width: {}", options.tab_width);
    println!(
        "continuation_indent_size: {}",
        options.continuation_indent_size
    );
    println!("end_of_line: {:?}", options.end_of_line);
    println!("insert_final_newline: {}", options.insert_final_newline);
    println!(
        "trim_trailing_whitespace: {}",
        options.trim_trailing_whitespace
    );
    println!("prefer_bom: {:?}", options.prefer_bom);
    println!("max_line_length: {:?}", options.max_line_length);
    println!("wrap: {:?}", options.wrap);
    println!(
        "preserve_single_line_blocks: {}",
        options.preserve_single_line_blocks
    );
    println!(
        "preserve_single_line_statements: {}",
        options.preserve_single_line_statements
    );
    println!("indent_case_contents: {}", options.indent_case_contents);
    println!("indent_switch_labels: {}", options.indent_switch_labels);
    println!("indent_braces: {}", options.indent_braces);
    println!("indent_labels: {:?}", options.indent_labels);
    println!("sort_usings: {:?}", options.sort_usings);
    println!("sources: {}", resolved.sources.len());
    println!("applied: {}", resolved.applied.len());
}

struct FileResult {
    path: PathBuf,
    input: String,
    output: String,
    changed: bool,
    warnings: Vec<String>,
}

fn process_file(
    path: &Path,
    resolver: &ConfigResolver,
    common: &CommonArgs,
) -> Result<Option<FileResult>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let Ok(input) = String::from_utf8(bytes) else {
        if common.verbose {
            eprintln!("skipping {} (not valid UTF-8)", path.display());
        }
        return Ok(None);
    };

    let resolved = resolve(resolver, path, common);
    let result = format_source(&input, &resolved.options);
    let changed = result.output != input;
    Ok(Some(FileResult {
        path: path.to_path_buf(),
        input,
        output: result.output,
        changed,
        warnings: resolved.options.warnings,
    }))
}

fn resolve(resolver: &ConfigResolver, path: &Path, common: &CommonArgs) -> ResolvedConfig {
    if common.no_config {
        ResolvedConfig::with_defaults()
    } else {
        resolver.resolve(path)
    }
}

fn run_parallel<F>(jobs: usize, files: &[PathBuf], f: F) -> Vec<Result<Option<FileResult>>>
where
    F: Fn(&Path) -> Result<Option<FileResult>> + Sync,
{
    let collect =
        || -> Vec<Result<Option<FileResult>>> { files.par_iter().map(|path| f(path)).collect() };
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

fn collect_files(paths: &[PathBuf], include_build_output: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
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
        let walker = builder.build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    eprintln!("warning: {err}");
                    continue;
                }
            };
            let is_cs = entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cs"));
            if entry.file_type().is_some_and(|kind| kind.is_file()) && is_cs {
                if !include_build_output && is_build_output(entry.path()) {
                    continue;
                }
                files.push(entry.into_path());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn is_build_output(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("obj")
    })
}

fn unified_diff(path: &Path, old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let old_header = format!("a/{}", path.display());
    let new_header = format!("b/{}", path.display());
    let mut unified = diff.unified_diff();
    unified.context_radius(3).header(&old_header, &new_header);
    unified.to_string()
}
