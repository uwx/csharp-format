# csharp-format

A **lossless, whitespace-only C# formatter** with **ReSharper "Reformat Code"
parity**. It reads the same configuration as your IDE — `.editorconfig`
(`csharp_*` and `resharper_*` keys) and ReSharper `*.DotSettings` — so a file
that ReSharper already formatted is left byte for byte unchanged.

Token text is never rewritten: the formatter only inserts, removes or moves
whitespace. Output is lossless and idempotent, and the same input and
configuration always produce the same bytes.

## Install

Requires the **.NET 10 SDK or newer** for `dotnet tool install`; the formatter
itself is a native binary with no .NET runtime dependency.

```sh
dotnet tool install -g csharp-format
```

Supported platforms: Windows x64, Linux x64, macOS arm64. The installer selects
the matching binary automatically.

## Usage

```sh
# Format files or directories in place (the subcommand is optional)
csharp-format format src/

# Check only: exits 1 when a file would change (useful in CI)
csharp-format check src/

# Show a unified diff instead of writing
csharp-format format --diff src/

# Format standard input
csharp-format stdin < File.cs
```

Common options: `-j/--jobs` for parallelism, `--no-config` to ignore
`.editorconfig`/`.DotSettings`, `--include-build-output` to also walk `bin/` and
`obj/`. Run `csharp-format format --help` for the full list.

## Configuration

Configuration is resolved per file, nearest file wins:

- `.editorconfig` — standard `indent_style`, `indent_size`, `end_of_line`,
  `max_line_length`, plus Roslyn (`csharp_*`) and ReSharper (`resharper_*`)
  keys.
- ReSharper `*.DotSettings` XML.
- Built-in ReSharper defaults when nothing is configured.

Inspect the result with `csharp-format config FILE` or
`csharp-format explain FILE`.

## Guarantees

- Whitespace-only: keywords, literals and comments are copied verbatim.
- Lossless: re-tokenizing the output yields the input token stream.
- Idempotent: formatting formatted output is a no-op.
- Fast and native: see the benchmark in the repository.
- Safe Rust: `unsafe_code` is denied workspace-wide.

## Links

- Usage guide: https://github.com/uwx/csharp-format/blob/main/USAGE.md
- Configuration reference: https://github.com/uwx/csharp-format/blob/main/USAGE.md#configuration
- Benchmarks: https://github.com/uwx/csharp-format/blob/main/BENCHMARK.md
- Issues and feedback: https://github.com/uwx/csharp-format/issues
- License: [MIT](https://github.com/uwx/csharp-format/blob/main/LICENSE)
