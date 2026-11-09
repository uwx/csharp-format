# AGENTS.md

Repository guide for coding agents and contributors. Read this before changing
the formatter.

## What this project is

`csharp-format` is a **lossless, whitespace-only C# formatter** written in Rust.
It reads `.editorconfig` / ReSharper `*.DotSettings` and produces text that
differs from the input only in whitespace. Its target is byte-for-byte parity
with ReSharper’s “Reformat Code” profile on already-formatted code.

## Non-negotiable invariants

Any change must preserve all of these:

1. **Whitespace-only** — token text is never rewritten. Do not normalize
   keywords, literal spelling, or comments.
2. **Lossless** — re-tokenizing the output yields exactly the input token
   stream. `csformat-core::verify_source` enforces this.
3. **Idempotent** — `format(format(x)) == format(x)`. `format_source` iterates
   to a fixed point and reverts to the input when no fixed point exists.
4. **Safe** — `unsafe_code` is denied workspace-wide. Do not introduce it.
   Library code (`csformat-core`) must not use `unwrap`/`expect` on fallible
   input; return errors or degrade gracefully.
5. **Deterministic** — output is a pure function of source text and resolved
   options.

## Build, test, verify

```sh
cargo build --release                 # binaries in target/release
cargo test --workspace                # unit + golden fixtures
cargo fmt --all -- --check            # formatting gate
cargo clippy --workspace --all-targets

# Full reference corpora (after fetching, see USAGE.md)
target/release/csformat-verify corpus/roslyn corpus/csharpier -j 8

# Golden fixtures only (also covered by cargo test)
target/release/csharp-format format --diff fixtures/basic.cs
```

Before committing, run at minimum:

```sh
cargo fmt --all
cargo test --workspace
cargo build --release
target/release/csformat-verify corpus/roslyn corpus/csharpier -j 8
```

The expected baseline is **all 1619 corpus files passing** (lossless,
idempotent, no new parse/lex errors).

## Repository map

| Path | Contents |
| --- | --- |
| `crates/csformat-core/src/lexer.rs` | Tokens plus the whitespace `Gap` before each token (comments/directives are trivia). |
| `crates/csformat-core/src/syntax.rs` | tree-sitter parse + per-token `AstInfo` (brace/paren/colon/comma/operator roles, member boundaries, `AttributePlacement`). |
| `crates/csformat-core/src/config.rs` | `.editorconfig` / `.DotSettings` parsing, glob matching, option structs, key handlers. |
| `crates/csformat-core/src/format.rs` | The emitter: `Layout`, `Frame`/`ParenFrame` stacks, `newlines_before`, `should_space`, `line_indent`, wrapping, fixed-point loop. |
| `crates/csformat-core/src/verify.rs` | Lossless/idempotent helpers used by tests and the harness. |
| `crates/csformat-cli/src/main.rs` | `csharp-format` CLI (`format`, `check`, `stdin`, `config`, `explain`). |
| `crates/csformat-verify/src/main.rs` | Corpus harness. |
| `fixtures/` + `fixtures/expected/` | Small inputs and ReSharper-verified expected outputs (golden test). |
| `scripts/jbcmp.sh` | Runs the real ReSharper CLI and diffs its output against ours. |
| `scripts/pack_dotnet_tool.py` | Builds RID-specific `.nupkg` packages so the binary installs with `dotnet tool install`. |
| `packaging/nuget-readme.md` | README embedded in every `.nupkg` (as `README.md`) and rendered on nuget.org. |
| `.github/workflows/release.yml` | CI: builds per-RID tarballs, packs the .NET tool packages, publishes the release (and nuget.org via trusted publishing when `NUGET_USER` is set). |
| `jbtmp/` | Scratch project for ReSharper CLI probes. `t.sln`/`t.csproj` must stay; other files are scratch. |
| `corpus/` | Reference corpora (not committed; 74 MB). |

## How the formatter works

The pipeline is: **lex → tree-sitter annotate → resolve config → emit**.

The emitter walks tokens in order. For each token it decides:

- `newlines_before(i)` — how many line breaks (0, 1, or 2+ blank lines).
- `should_space(i)` — whether a single space separates it from the previous
  token.
- `line_indent(i)` — the **column** (not level) of indentation when the token
  starts a line.
- `should_wrap(i)` — whether to insert a break to respect `max_line_length`.

Indentation is column-based: `Frame.open_indent`, `Frame.content_indent`,
`ParenFrame.indent`, `last_code_indent` and `line_indent` all carry columns.
`write_indent` converts columns to spaces/tabs. Keep it that way; mixing levels
and columns silently breaks alignment.

Existing continuation lines keep their **source column** (this is what makes an
already-ReSharper-formatted corpus a no-op). Breaks that the formatter
introduces itself use `continuation_indent_size` / `wrap.continuation_indent`.

`format_source` formats once and, if the output changed, iterates up to three
more passes until the output is a fixed point. If it never converges it returns
the original source with `changed = false`.

## Adding a formatting rule

1. **Observe ReSharper first.** Add a minimal probe to `jbtmp/A.cs` and run:
   ```sh
   (cd jbtmp && dotnet jb cleanupcode t.sln --no-build --profile="Built-in: Reformat Code")
   diff -u <probe> jbtmp/A.cs
   ```
   Do not implement a rule from intuition; measure the exact default.
2. **Decide where it belongs.** Structural facts (node kinds, brace owner,
   colon/comma kind, declaration boundaries) go in `syntax.rs`; whitespace
   policy goes in `format.rs`; parsing goes in `config.rs`.
3. **Add a fixture.** Put the input in `fixtures/<name>.cs` and the ReSharper
   output in `fixtures/expected/<name>.cs`. The golden test
   (`tests/golden.rs`) compares our output byte-for-byte.
4. **Keep idempotency.** Patterns that depend on the shape of the previous pass
   (for example “member spans several lines”) must converge; the fixed-point
   loop is a safety net, not a license to oscillate. Prefer decisions based on
   the emitted layout over source line breaks where possible.
5. **Re-run the full verification set** listed above.

## ReSharper probing notes

- CLI: `dotnet tool restore`, then `dotnet jb cleanupcode t.sln --no-build
  --profile="Built-in: Reformat Code"`. `--no-build` counts as the build
  argument; do not omit it.
- Useful defaults already established (verify with a probe before relying on
  them): at most 2 blank lines; blank lines before `{` and between `}` and
  `else`/`catch`/`finally` are preserved; `#region` is indented with code while
  `#if` stays at column 0; single-line `try`/`if`/`lock`/`using` blocks are
  expanded; initializers, lambda bodies and accessor bodies may stay inline.
- Attribute placement: fields stay on the declaration line; properties,
  indexers and events only when the whole declaration is single-line; a doc
  comment forces them onto their own line; an attribute list of 39+ characters
  moves the member down (default limit 40).
- Switch sections: `break;` keeps its authored line; a `{` directly under a
  `case` label aligns with the label.
- Local functions: separating blank lines appear only around multi-line ones.
- `..` range spacing and the ambiguous `(x)+y` / `(x)*y` cast forms keep the
  source spacing.

## Gotchas

- `tree-sitter-c-sharp` is pinned to a git revision (`f2dea45…`) for C# 14
  `extension` support. The build needs network access the first time;
  `Cargo.lock` must stay committed.
- Do not change `should_space` ordering casually: branches are order-sensitive
  (e.g. generic-close handling must run before binary-operator handling).
- Comments and directives are trivia; `next_code`/`prev_code` skip them. Use
  those helpers rather than `i ± 1` when the previous token may be trivia.
- `format_source` is intentionally slower for changed files (it stabilizes the
  output). Do not “optimize” away the second pass without re-checking
  idempotency on the corpus.
- Scratch artifacts: `.tmp/` is ignored; `jbtmp/` is committed except probe
  files (`A.cs`, `orig.cs`, etc. are ignored). Do not commit corpus data or
  `target/`.
- Windows paths with `:` in diff filenames cannot be opened by some tools; use
  numbered diff files instead.

## Commit expectations

- Keep changes focused; do not reformat unrelated files.
- `cargo fmt` before committing.
- Mention in the commit message which behaviour changed and the ReSharper
  evidence (probe result or fixture name).
- Never commit secrets, `.env` files, or generated corpora.
