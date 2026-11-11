# csharp-format

A **lossless C# formatter** written in Rust that follows **Roslyn** (`csharp_*`)
and **ReSharper** (`resharper_*`, `.DotSettings`) configuration.

The emitter only inserts, removes or moves **whitespace**. Token text is never
rewritten, so output is always token-for-token identical to the input, and
`format(format(x)) == format(x)`.

Targets modern C# (C# 13–15 / .NET 9–11 syntax): raw strings, collection
expressions, primary constructors, file-scoped namespaces, `with` expressions
and C# 14 `extension` blocks.

## Features

- **Whitespace-only and lossless** — tokens are copied verbatim; only the gaps
  between them change.
- **Idempotent** — formatting is iterated to a fixed point, so a second run is
  always a no-op.
- **Roslyn + ReSharper configuration** — `.editorconfig` (`csharp_*`,
  `resharper_*`, `dotnet_*`, standard indent/EOL keys) and ReSharper
  `*.DotSettings` XML, resolved per file with nearest-wins precedence.
- **ReSharper Reformat Code parity** — verified against a real
  ReSharper-formatted codebase (see [Status](#status)).
- **Fast** — a native binary; about 40 ms for a 992-line file, versus 0.45 s
  for CSharpier, 1.9 s for `dotnet format whitespace` and 10.2 s for the
  ReSharper CLI on the same machine (see [BENCHMARK.md](BENCHMARK.md)).
- **Practical CLI** — format in place, check (exit code for CI), unified diff,
  stdin, resolved-config inspection, and a corpus verification harness.
- **Safe Rust** — `unsafe_code` is denied workspace-wide; library paths avoid
  `unwrap`.

## Status

| Check | Result |
| --- | --- |
| CSharpier + Roslyn corpora (1619 files, `.editorconfig` applied) | 1619/1619 pass — 0 lossless, 0 idempotency, 0 parse/lex regressions |
| Same corpora with `--no-config` (built-in defaults) | 1619/1619 pass, identical counters |
| ReSharper CLI "Reformat Code" comparison (15 fixtures) | 15/15 byte-identical |
| NFM-World — NFM-authored projects (896 files) | 890 byte-identical (99.3%); the 6 exceptions are sources that are not actually ReSharper-formatted (stale inline blocks or trailing blank lines) |
| `cargo test` | 37 tests pass (35 unit + 2 golden) |

## Install

### .NET tool

RID-specific tool packages need the **.NET 10 SDK or newer**. Windows x64,
Linux x64 and macOS arm64 builds are packaged; the installer picks the right
binary for the current machine.

```sh
# once published to nuget.org
dotnet tool install -g csharp-format

# or straight from the GitHub release assets
gh release download --pattern '*.nupkg' --dir nupkg
dotnet tool install -g csharp-format --version <version> --add-source ./nupkg
```

### From source

```sh
git clone <repository-url>
cd csharp-format
cargo build --release
```

The binary is `target/release/csharp-format`; the corpus harness is
`target/release/csformat-verify`.

Requirements:

- Rust 1.85+ (edition 2024).
- A network connection on the first build: the C# grammar is pinned to a
  `tree-sitter/tree-sitter-c-sharp` git revision that supports C# 14
  (revision `f2dea45`). `Cargo.lock` records the exact revision.
- The .NET 10 SDK only for `dotnet tool install` and for the optional
  ReSharper comparison tooling in `jbtmp/` (`dotnet jb cleanupcode`). The
  binary itself has no .NET dependency.

## Quick start

```sh
# Format files/directories in place (no subcommand also formats)
csharp-format format src/ -j 0

# Check only; exits 1 when something would change
csharp-format check src/

# Preview a diff without writing
csharp-format format --diff src/Program.cs

# Format stdin, resolving config as if the input were src/Program.cs
csharp-format stdin --file-name src/Program.cs < src/Program.cs

# Inspect the resolved options and where they came from
csharp-format config src/Program.cs --json
csharp-format explain src/Program.cs
```

Files under `bin/` and `obj/` are skipped by default (case-insensitive) and
directory walks respect `.gitignore`. Pass `--include-build-output` to include
build output. Non-UTF-8 files are skipped and counted instead of failing.

See **[USAGE.md](USAGE.md)** for the full command reference, configuration
guide, CI integration and troubleshooting.

## Configuration

Configuration is discovered from the filesystem root down to each source
file's directory (nearest wins):

1. `.editorconfig` — standard keys plus `csharp_*` (Roslyn),
   `resharper_*` / `resharper_csharp_*` (ReSharper) and `dotnet_*` keys.
2. `*.DotSettings` — ReSharper's XML settings format, applied after
   `.editorconfig`.

Example:

```ini
root = true

[*.cs]
indent_style = space
indent_size = 4
insert_final_newline = true

csharp_new_line_before_open_brace = all
csharp_space_around_binary_operators = before_and_after
csharp_keep_blank_lines_in_code = 2

resharper_csharp_max_line_length = 130
resharper_csharp_wrap_arguments_style = chop_if_long
resharper_csharp_brace_style = next_line
```

`.editorconfig`/`.DotSettings` support covers the keys listed in
[USAGE.md](USAGE.md#supported-keys). Everything in the ReSharper whitespace
family is at least parsed: keys without a dedicated handler are accepted and
reported with `-v` as "accepted but not yet granularly implemented" rather than
being treated as unknown.

## Architecture

```
source ──▶ lexer ──▶ syntax (tree-sitter) ──▶ format (emitter) ──▶ output
             │             │                        │
             │             └─ brace/paren/colon/comma/operator roles,
             │                generic angle brackets, parse-error tolerance
             └─ tokens + trivia gaps (newlines, comments, directives)
```

| Crate | Purpose |
| --- | --- |
| `csformat-core` | Lexer, tree-sitter annotation, config resolution, formatting engine, verification helpers |
| `csformat-cli` | `csharp-format` binary: `format`, `check`, `stdin`, `config`, `explain` |
| `csformat-verify` | Corpus harness checking losslessness, idempotency and parse/lex regressions |

1. **Lexer** (`csformat-core/src/lexer.rs`) splits source into tokens plus the
   whitespace gap preceding each token, keeping comments and preprocessor
   directives as trivia. All C# literal forms are handled, including verbatim,
   interpolated and raw strings.
2. **Syntax** (`syntax.rs`) parses with `tree-sitter-c-sharp` and annotates each
   token with structural roles: inline vs multi-line braces, paren kind, colon
   kind, comma kind, binary/unary operator, generic `<`/`>`, member boundaries.
   Parse errors are tolerated; annotations degrade gracefully and formatting
   continues.
3. **Config** (`config.rs`) resolves the option set from `.editorconfig` /
   `.DotSettings` using glob matching and brace expansion, tracking every
   applied setting for `config`/`explain`.
4. **Format** (`format.rs`) emits tokens in order with brace and paren stacks,
   column-based indentation, continuation alignment, blank-line policies and
   speculative wrapping driven by `max_line_length`.
5. **Verify** (`verify.rs`) re-tokenizes input and output and asserts token
   equality (whitespace ignored), which the corpus harness and golden tests use.

## Verification

```sh
# Unit + golden tests
cargo test --workspace

# Formatting hygiene
cargo fmt --all -- --check

# Full corpora (dotnet/roslyn + belav/csharpier checked out under corpus/)
cargo build --release -p csformat-verify
./target/release/csformat-verify --json

# Or with built-in defaults only
./target/release/csformat-verify --no-config

# Single file/dir, treating pre-existing parse errors as failures
./target/release/csformat-verify path/to/file.cs --strict
```

The harness checks, per file: formatting succeeds without panicking, no token
text is lost or changed, output is idempotent, and the output does not introduce
tree-sitter parse errors that the input did not already have. CSharpier's XML
formatting fixtures (`xml_ignore`, `xml_strict`) are skipped because they are
not C# sources.

ReSharper compatibility can be re-checked with:

```sh
scripts/jbcmp.sh fixtures/basic.cs
```

which formats the file with the local ReSharper CLI (`dotnet jb cleanupcode`,
global tool `jetbrains.resharper.globaltools` 2026.2.4.1, profile
"Built-in: Reformat Code"), stores its output under `fixtures/expected/`, and
diffs it against our formatter.

## Performance

Measured on a 992-line NFM-World file (`Mad/Gameplay/RacePhase.cs`), best of
three runs on the same machine. Full matrix (20-file runs, dirty input,
correctness checks, methodology) in [BENCHMARK.md](BENCHMARK.md).

| Formatter | 1 file | 20 files (~20k lines) |
| --- | ---: | ---: |
| `csharp-format format` | **41 ms** | **92 ms** |
| `csharpier format` (1.3.0) | 451 ms | 632 ms |
| `dotnet-fastformat` (1.5.0) | 595 ms | 882 ms |
| `dotnet format whitespace` | 1,922 ms | 2,223 ms |
| ReSharper CLI `jb cleanupcode` (2026.2.4.1) | 10,227 ms | 15,827 ms |

All figures are end-to-end command times and include process startup; the .NET
tools pay a runtime/workspace-load cost before they touch a file, while our
binary is native. Formatting a further copy of the file costs about 2.7 ms.

On already-formatted input `csharp-format` reproduces the file byte for byte
(ReSharper and the Roslyn formatters also leave it alone; CSharpier reprints it
to its own style). That no-op property is a test, not just a claim:
`fixtures/noop_layout.cs` is a ReSharper-formatted file the formatter must not
touch.

## Design invariants

- **Whitespace-only**: token text is never rewritten.
- **Lossless**: output re-tokenizes to exactly the input token stream.
- **Idempotent**: when the first pass changes a file it is formatted again
  until the output is a fixed point (at most 3 additional passes); if no fixed
  point is reached, the original source is returned unchanged.
- **Deterministic**: output is a pure function of source and options.
- **Safe Rust**: `unsafe_code = deny`; no `unwrap` in library paths.

## Known limitations

- Nothing outside whitespace is changed: members are not reordered, expressions
  are not rewritten and redundant code is not removed.
- Continuation lines that the author already broke keep their source column
  (this is what makes an already-ReSharper-formatted corpus a no-op). Lines the
  formatter breaks itself use the configured continuation indent.
- Wrapping is a heuristic: it breaks at argument/parameter commas, binary
  operators, assignments and member accesses when a line would exceed the
  configured limit, but it does not implement every ReSharper wrapping group.
- Vendored third-party directories in NFM-World (kni, Maxine.*, MemoryPack,
  Facepunch.Steamworks, FontStashSharp, websocket-sharp, NAudio.Flac,
  Apos.Shapes, FixedMathSharp, Poly2Tri, LibTessDotNet, SignalsDotnet,
  Microsoft.UI.Reactor.Yoga) are not ReSharper-formatted and are out of scope
  for the parity target.
- Files that already contain syntax errors are formatted on a best-effort basis
  and are not treated as failures unless `--strict` is used.

## Publishing

Releases are produced by `.github/workflows/release.yml`. Pushing a `v*` tag
creates a GitHub release with per-RID tarballs and the `.nupkg` tool packages,
and publishes those packages to nuget.org when trusted publishing is
configured. Pushes to `main` produce a rolling nightly release and never
publish to nuget.org.

Trusted publishing uses short-lived OIDC credentials, so no NuGet API key is
stored in the repository:

1. On nuget.org, open **Account → Trusted Publishing** and add a policy for
   this repository with the workflow file `release.yml` (the file name only,
   not the `.github/workflows/` path). Leave the environment field empty.
2. In the GitHub repository, add an Actions **variable** (not a secret)
   `NUGET_USER` containing your nuget.org profile name, not your email
   address. Without it the NuGet steps are skipped and only GitHub release
   assets are produced.
3. Push a `v*` tag. `NuGet/login@v1` exchanges the job's OIDC token for a
   one-hour API key, which the workflow uses to push the RID packages first
   and the pointer package last.

Every package embeds [packaging/nuget-readme.md](packaging/nuget-readme.md) as
`README.md`; nuget.org renders it on the package page. That readme is written
for package consumers (install, usage, guarantees), not for contributors.

## Contributing

See **[AGENTS.md](AGENTS.md)** for the repository layout, invariants and the
verification checklist to run before committing. Bug reports should include the
input file, the resolved configuration (`csharp-format config --json FILE`) and
the diff produced by `csharp-format format --diff FILE`.

## License

[MIT](LICENSE) © 2026 csharp-format contributors
