# Usage guide

This guide covers installation, every command and option, configuration,
verification and CI integration. For an overview and benchmark figures see
[README.md](README.md); for repository conventions see [AGENTS.md](AGENTS.md).

## Contents

- [Install](#install)
- [Commands](#commands)
  - [format](#format)
  - [check](#check)
  - [stdin](#stdin)
  - [config](#config)
  - [explain](#explain)
- [File selection](#file-selection)
- [Exit codes](#exit-codes)
- [Configuration](#configuration)
  - [Discovery and precedence](#discovery-and-precedence)
  - [Supported keys](#supported-keys)
  - [ReSharper aliases](#resharper-aliases)
  - [DotSettings](#dotsettings)
  - [Examples](#examples)
- [Verification harness](#verification-harness)
- [Comparing against ReSharper](#comparing-against-resharper)
- [CI and editor integration](#ci-and-editor-integration)
- [Troubleshooting](#troubleshooting)

## Install

### .NET tool

RID-specific tool packages need the **.NET 10 SDK or newer**. The release
workflow publishes Windows x64, Linux x64 and macOS arm64 packages; the
installer selects the binary for the current platform.

```sh
# from nuget.org, once published
dotnet tool install -g csharp-format

# from the GitHub release assets
gh release download --pattern '*.nupkg' --dir nupkg
dotnet tool install -g csharp-format --version <version> --add-source ./nupkg

csharp-format --version
```

Building the packages locally:

```sh
python3 scripts/pack_dotnet_tool.py --version 0.1.0 --out dist/nupkg \
  --binary win-x64=target/release/csharp-format.exe
```

### From source

```sh
git clone <repository-url>
cd csharp-format
cargo build --release
```

Binaries:

| Binary | Path |
| --- | --- |
| `csharp-format` | `target/release/csharp-format` (`.exe` on Windows) |
| `csformat-verify` | `target/release/csformat-verify` (`.exe` on Windows) |

Put `target/release` on `PATH`, or install the CLI with

```sh
cargo install --path crates/csformat-cli --locked
```

Requirements:

- Rust 1.85 or newer (edition 2024).
- Network access on the first build. `tree-sitter-c-sharp` is pinned to git
  revision `f2dea45` because released 0.23.5 does not parse C# 14 `extension`
  blocks. `Cargo.lock` records the revision, so later builds are reproducible.
- The .NET 10 SDK only for the optional ReSharper comparison workflow.

A release archive can also be produced by the GitHub Actions workflow
(`.github/workflows/release.yml`): pushing to `main` updates a rolling
`nightly` prerelease, pushing a `v*` tag creates a normal release, and manual
`workflow_dispatch` runs do the same as a `main` push.

## Commands

Running `csharp-format` with no subcommand is equivalent to `format`.

### `format`

Formats files in place by default.

```text
csharp-format format [OPTIONS] [PATH]...
```

| Option | Meaning |
| --- | --- |
| `[PATH]...` | Files or directories (defaults to the current directory). |
| `--no-config` | Ignore `.editorconfig`/`.DotSettings`; use built-in defaults. |
| `--stdout` | Write the formatted result to stdout instead of modifying files. |
| `--diff` | Print a unified diff instead of writing files. |
| `-v, --verbose` | Print configuration warnings and per-file diagnostics. |
| `--include-build-output` | Also walk `bin/` and `obj/`. |
| `-j, --jobs <JOBS>` | Worker threads; `0` uses all CPUs. |

Examples:

```sh
csharp-format format src/
csharp-format format --diff src/Program.cs
csharp-format format --stdout src/Program.cs > formatted.cs
csharp-format format --no-config src/
```

`--stdout` and `--diff` are mutually exclusive.

### `check`

Formats in memory and reports files that would change. Writes nothing.

```text
csharp-format check [OPTIONS] [PATH]...
```

| Option | Meaning |
| --- | --- |
| `--no-config` | Ignore configuration files. |
| `-q, --quiet` | Only set the exit status; print nothing. |
| `-v, --verbose` | Configuration warnings and per-file diagnostics. |
| `--include-build-output` | Also walk `bin/` and `obj/`. |
| `-j, --jobs <JOBS>` | Worker threads. |

```sh
csharp-format check src/
csharp-format check -q src/ && echo "clean"
```

### `stdin`

Formats standard input and writes the result to stdout. Configuration is
resolved relative to `--file-name` when given, otherwise built-in defaults are
used.

```text
csharp-format stdin [OPTIONS]
```

| Option | Meaning |
| --- | --- |
| `--file-name <PATH>` | Resolve configuration as if the input came from this path. |
| `--check` | Exit 1 when the input is not already formatted. |
| `--diff` | Print a unified diff instead of the formatted source. |
| `--no-config` | Ignore configuration files. |
| `-v, --verbose` | Configuration warnings. |

```sh
csharp-format stdin --file-name src/Program.cs < src/Program.cs
csharp-format stdin --check --file-name src/Program.cs < src/Program.cs
```

### `config`

Prints the fully resolved options for a path, including which files
contributed (`sources`) and how many settings were applied.

```text
csharp-format config [--json] [PATH]
```

```sh
csharp-format config src/Program.cs
csharp-format config --json src/Program.cs
```

### `explain`

Prints every configuration file that applies to a path and the settings it
contributes.

```sh
csharp-format explain src/Program.cs
```

## File selection

- Arguments may be files or directories; directories are walked recursively.
- `.gitignore` rules are honoured while walking, as are hidden-file rules from
  the `ignore` crate.
- `bin/` and `obj/` directories are skipped by default, case-insensitively.
  Pass `--include-build-output` to include them. If a file inside `bin/`/`obj/`
  is passed explicitly, it is also skipped unless the flag is set.
- Files that are not valid UTF-8 are skipped and counted (reported in verbose
  output and in `check`'s summary) rather than causing an error.
- Files are processed in parallel; output order of changed-file lists is
  deterministic.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success; nothing to do (for `check`/`stdin --check`: input is already formatted). |
| `1` | `check` or `stdin --check` found files/input that would change. |
| `2` | Error (bad arguments, unreadable path, I/O failure). |

## Configuration

### Discovery and precedence

Configuration is collected from the filesystem root down to the source file's
directory. Later (nearer) files override earlier ones:

1. `.editorconfig` files. `root = true` stops the walk upward. Glob sections
   (`[*.cs]`, `[src/**/*.cs]`, `[{a,b}.cs]`) are matched against the path
   relative to the file that declared them.
2. `*.DotSettings` files (ReSharper XML), applied after `.editorconfig`, in the
   same root-to-leaf order.

Configuration is resolved per file, so one invocation can format files from
several projects with different settings.

### Supported keys

Standard `.editorconfig` keys:

`indent_style`, `indent_size`, `tab_width`, `end_of_line`, `charset`,
`insert_final_newline`, `trim_trailing_whitespace`, `max_line_length`,
`continuation_indent_size`.

Roslyn (`csharp_*`):

- `csharp_new_line_before_open_brace` (`all`, `none`, or a comma list of
  `methods,types,properties,accessors,anonymous_methods,anonymous_types,initializers,control_blocks`)
- `csharp_new_line_before_else`, `csharp_new_line_before_catch`,
  `csharp_new_line_before_finally`
- `csharp_new_line_before_members_in_initializers`,
  `csharp_new_line_before_members_in_anonymous_types`,
  `csharp_new_line_between_query_expression_clauses`
- `csharp_space_after_cast`, `csharp_space_around_binary_operators`
  (`before_and_after`, `none`, `ignore`), `csharp_space_after_comma`,
  `csharp_space_before_comma`, `csharp_space_after_dot`,
  `csharp_space_before_dot`, `csharp_space_after_semicolon_in_for_statement`,
  `csharp_space_before_semicolon_in_for_statement`,
  `csharp_space_after_keywords_in_control_flow_statements`,
  `csharp_space_between_method_call_*parentheses`,
  `csharp_space_between_method_declaration_*parentheses`,
  `csharp_space_before_open_square_brackets`,
  `csharp_space_between_square_brackets`,
  `csharp_space_between_empty_square_brackets`,
  `csharp_space_before/after_colon_in_conditional`,
  `csharp_space_before/after_colon_in_inheritance_clause`,
  `csharp_space_before/after_question_in_conditional`
- `csharp_indent_braces`, `csharp_indent_case_contents`,
  `csharp_indent_switch_labels`, `csharp_indent_labels`
  (`one_less_than_current`, `flush_left`, `no_change`)
- `csharp_preserve_single_line_blocks`, `csharp_preserve_single_line_statements`
- `csharp_keep_blank_lines_in_code`,
  `csharp_keep_blank_lines_in_declarations`
- `csharp_max_attribute_length_for_same_line`
- `dotnet_sort_system_directives_first`, `dotnet_separate_import_directive_groups`

ReSharper (`resharper_*`):

- `resharper_brace_style` (`next_line`/`allman`, `end_of_line`/`k_r`/`kr`/`none`)
- `resharper_continuation_indent_size`, `resharper_max_line_length`,
  `resharper_wrap_lines`
- `resharper_blank_lines_between_members`,
  `resharper_blank_lines_around_region`,
  `resharper_blank_lines_after_region`,
  `resharper_blank_lines_before_endregion`
- `resharper_keep_blank_lines_in_code`,
  `resharper_keep_blank_lines_in_declarations`
- `resharper_max_attribute_length_for_same_line`

Keys in the ReSharper whitespace family (`resharper_space_*`,
`resharper_new_line_*`, `resharper_indent_*`, `resharper_wrap_*`,
`resharper_align_*`, `resharper_blank_lines_*`, `resharper_min_blank_lines_*`,
`resharper_keep_blank_lines_*`, `resharper_max_*`) that do not have a dedicated
handler are still parsed and are reported with `-v` as “accepted but not yet
granularly implemented” instead of being treated as unknown.

### ReSharper aliases

ReSharper emits language-specific aliases with a `resharper_csharp_` prefix.
They are normalised onto the shared form, so these are equivalent:

```ini
resharper_csharp_brace_style = next_line
resharper_brace_style = next_line
```

The same applies to `resharper_csharp_max_line_length`,
`resharper_csharp_keep_blank_lines_in_code`, and so on.

### DotSettings

`*.DotSettings` files store settings as XML leaf keys. Leaf names are mapped
onto the same option set, for example:

```xml
<wpf:ResourceDictionary xmlns:wpf="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
  <s:String x:Key="/Default/CodeStyle/CodeFormatting/CSharpFormat/INDENT_SIZE">4</s:String>
  <s:String x:Key="/Default/CodeStyle/CodeFormatting/CSharpFormat/MAX_LINE_LENGTH">130</s:String>
  <s:String x:Key="/Default/CodeStyle/CodeFormatting/CSharpFormat/BRACE_STYLE">NEXT_LINE</s:String>
</wpf:ResourceDictionary>
```

### Examples

Roslyn-style project:

```ini
root = true

[*.cs]
indent_style = space
indent_size = 4
end_of_line = lf
insert_final_newline = true
trim_trailing_whitespace = true

csharp_new_line_before_open_brace = all
csharp_space_around_binary_operators = before_and_after
csharp_indent_case_contents = true
csharp_indent_switch_labels = false
csharp_keep_blank_lines_in_code = 1
```

ReSharper-style project:

```ini
[*.cs]
resharper_csharp_brace_style = next_line
resharper_csharp_max_line_length = 130
resharper_csharp_wrap_arguments_style = chop_if_long
resharper_csharp_wrap_before_binary_opsign = true
resharper_csharp_keep_blank_lines_in_declarations = 2
```

Per-directory override:

```ini
[src/generated/**/*.cs]
# Generated files keep their layout empty of extra blank lines.
resharper_blank_lines_between_members = 0
```

## Verification harness

`csformat-verify` formats each file in memory and checks four properties:

1. formatting does not panic or fail;
2. output re-tokenizes to exactly the input tokens (lossless);
3. formatting the output again produces the same bytes (idempotent);
4. the output does not introduce new tree-sitter parse or lex errors.

```text
csformat-verify [OPTIONS] [PATH]...
```

| Option | Meaning |
| --- | --- |
| `[PATH]...` | Files/directories; defaults to `corpus/roslyn` and `corpus/csharpier`. |
| `--no-config` | Use built-in defaults instead of `.editorconfig`/`.DotSettings`. |
| `--strict` | Treat files whose input already has lex/parse errors as failures. |
| `--json` | Machine-readable summary. |
| `--max-report <N>` | Maximum number of failing files to print (default 25). |
| `--include-build-output` | Also verify `bin/` and `obj/`. |
| `-j, --jobs <JOBS>` | Worker threads. |

```sh
csformat-verify --json
csformat-verify path/to/repo --strict
```

Fetch the corpora separately (they are not committed):

```sh
mkdir -p corpus
git clone --depth 1 https://github.com/dotnet/roslyn corpus/roslyn
git clone --depth 1 https://github.com/belav/csharpier corpus/csharpier
```

## Comparing against ReSharper

The repository contains a scratch project under `jbtmp/` used to run the real
ReSharper CLI:

```sh
dotnet tool restore
scripts/jbcmp.sh fixtures/basic.cs
```

`scripts/jbcmp.sh` copies the file to `jbtmp/A.cs`, runs
`dotnet jb cleanupcode t.sln --no-build --profile="Built-in: Reformat Code"`,
stores the result under `fixtures/expected/`, and diffs our output against it.
`jbtmp/t.sln` and `jbtmp/t.csproj` must exist; the tool manifest
(`dotnet-tools.json`) pins `jetbrains.resharper.globaltools` 2026.2.4.1.

Probe workflow for a new formatting rule:

```sh
cp my-probe.cs jbtmp/A.cs
(cd jbtmp && dotnet jb cleanupcode t.sln --no-build --profile="Built-in: Reformat Code")
diff -u my-probe.cs jbtmp/A.cs
```

ReSharper’s output is the source of truth; add a fixture under `fixtures/` and
its expected output under `fixtures/expected/` so the golden test covers it.

## CI and editor integration

### GitHub Actions

A ready-made workflow lives in `.github/workflows/release.yml`. It runs
`cargo fmt --check` and the test suite, builds release binaries on Linux,
Windows and macOS, uploads them as artifacts, and creates/updates a GitHub
Release:

- push to `main` → updates the rolling `nightly` prerelease;
- push of a `v*` tag → creates that release;
- `workflow_dispatch` → same as a `main` push.

Minimal formatting gate for another project:

```yaml
- uses: actions/checkout@v4
- uses: dtolnay/rust-toolchain@stable
- run: cargo build --release --locked
- run: ./target/release/csharp-format check -q src/
```

### Pre-commit hook

```sh
#!/bin/sh
# .git/hooks/pre-commit
cargo run --quiet --release -p csformat-cli -- check -q .
```

### Editor integration

Any editor that can run a shell command can use `csharp-format format` on the
current file, or `csharp-format stdin --file-name <path>` for on-type/save
formatting without a temporary file.

## Troubleshooting

**The first build fails while fetching `tree-sitter-c-sharp`.**
The grammar is a git dependency and needs network access once. Check
`Cargo.lock` was committed; use `cargo build --locked` on CI.

**C# 14 `extension` blocks produce parse errors.**
You are building against tree-sitter-c-sharp 0.23.5 instead of the pinned git
revision. Run `cargo update -p tree-sitter-c-sharp` or check that `Cargo.toml`
still pins `rev = "f2dea45d6f6998839f2617c5137624654a58fa80"`.

**A file is reported as changed but looks identical.**
Trailing whitespace, final-newline or blank-line policies differ. Use
`csharp-format format --diff FILE` to see exactly what changes, and
`csharp-format config FILE` to see the resolved options.

**A file is skipped.**
Non-UTF-8 files are skipped and counted. Files under `bin/`/`obj/` are skipped
unless `--include-build-output` is given; `.gitignore`d files are skipped while
walking directories.

**Formatting is not idempotent for a particular file.**
Please report it. The formatter iterates to a fixed point and falls back to the
original source if it cannot converge; `csformat-verify FILE` reports the
failing property. Include the file and resolved configuration in the report.
