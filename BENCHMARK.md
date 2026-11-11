# Benchmark

Wall-clock comparison of `csharp-format` against the formatters it is normally
compared with: `dotnet format whitespace`, [FastFormat], [CSharpier] and the
ReSharper command-line tool.

The headline: on a 992-line file `csharp-format` takes **41 ms**; the next
fastest tool takes **0.45 s** and the slowest **10.2 s**. Most of the gap is
process startup — our formatter is a native binary, the others start a .NET
runtime (and, for `dotnet format` and ReSharper, load an MSBuild workspace).
Throughput is where the design shows: formatting a second copy of the file
costs about **2.7 ms**.

[FastFormat]: https://github.com/lyssieth/dotnet-fastformat
[CSharpier]: https://csharpier.com/

## Environment

| | |
| --- | --- |
| OS | Windows 11 (build 28000), Git Bash |
| CPU | AMD Ryzen 7 9800X3D (8 cores / 16 threads) |
| Rust | 1.99.1 |
| .NET SDK | 11.0.100-rc.3 |

| Tool | Version | Invocation |
| --- | --- | --- |
| `csharp-format` | 0.1.0 | `csharp-format format <dir>` |
| CSharpier | 1.3.0 | `csharpier format <dir>` |
| FastFormat | 1.5.0 | `dotnet-fastformat --force <dir>` |
| `dotnet format` | SDK 11.0.100-rc.3 | `dotnet format whitespace <proj> --no-restore --verbosity quiet` |
| ReSharper CLI | 2026.2.4.1 | `dotnet jb cleanupcode t.sln --no-build --profile="Built-in: Reformat Code"` |

## Input

`RacePhase.cs` from the NFM-World game, a 992-line / 38,537-byte file (UTF-8
with BOM, LF) that was produced by ReSharper and is treated as the reference
output. The project's `.editorconfig` sits next to every copy of the file; it
sets `max_line_length = 130`, `chop_if_long` argument/parameter/chained-binary
wrapping and `align_multiline_argument/parameter = false`.

* **formatted** — the file as-is.
* **dirty** — the same file with every line's leading spaces and tabs removed
  (992 lines, 31,249 bytes). Line breaks, comments and tokens are unchanged.
* **20 files** — 20 copies of the file (19,840 lines, ~771 KB).

The `dirty` variant is deliberately harsh: it removes indentation but keeps the
authored line breaks, so a formatter cannot recover alignment purely from the
syntax tree. Treat it as an upper bound on work, not a realistic workflow.

## Method

* Wall clock around the whole command, including process startup.
* Best of three runs; a fresh copy of the input is written before every run.
* Warm caches: the .NET projects are restored and the ReSharper solution has
  been loaded at least once before timing.
* ReSharper needs a solution, so its runs use a scratch project inside
  `jbtmp/`. The same `.editorconfig` is placed next to that project so the
  settings match the other tools.
* `dotnet format` and ReSharper were given a real `.csproj` / `.sln`; they do
  workspace and semantic analysis that `csharp-format` does not.

## Results

Best of three runs, milliseconds:

| Tool | 1 file, formatted | 1 file, dirty | 20 files, formatted | 20 files, dirty |
| --- | ---: | ---: | ---: | ---: |
| **csharp-format 0.1.0** | **41** | **56** | **92** | **147** |
| CSharpier 1.3.0 | 451 | 457 | 632 | 642 |
| FastFormat 1.5.0 | 595 | 668 | 882 | 1,034 |
| `dotnet format whitespace` | 1,922 | 1,985 | 2,223 | 2,395 |
| ReSharper CLI | 10,227 | 10,264 | 15,827 | 17,272 |

Relative to `csharp-format` on the formatted inputs: CSharpier 11.0×,
FastFormat 14.5×, `dotnet format` 46.9×, ReSharper 249× (1 file); 6.9×, 9.6×,
24.2× and 172× (20 files). The single-file ratios are dominated by process
startup and are the noisiest numbers here.

Per file on the 20-file dirty input: `csharp-format` **7.4 ms**, CSharpier
32 ms, FastFormat 52 ms, `dotnet format` 120 ms, ReSharper 864 ms. The marginal
cost of each additional file (formatted input) is about **2.7 ms** for
`csharp-format`, versus 9.5 ms (CSharpier), 15.1 ms (FastFormat), 15.8 ms
(`dotnet format`) and 295 ms (ReSharper).

### All runs

| Tool | Cell | Run 1 | Run 2 | Run 3 |
| --- | --- | ---: | ---: | ---: |
| csharp-format | 1 file, formatted | 52 | 41 | 41 |
| csharp-format | 1 file, dirty | 57 | 56 | 57 |
| csharp-format | 20 files, formatted | 93 | 92 | 94 |
| csharp-format | 20 files, dirty | 153 | 159 | 147 |
| CSharpier | 1 file, formatted | 739 | 451 | 458 |
| CSharpier | 1 file, dirty | 460 | 460 | 457 |
| CSharpier | 20 files, formatted | 632 | 662 | 663 |
| CSharpier | 20 files, dirty | 669 | 642 | 673 |
| FastFormat | 1 file, formatted | 790 | 595 | 636 |
| FastFormat | 1 file, dirty | 674 | 690 | 668 |
| FastFormat | 20 files, formatted | 913 | 882 | 932 |
| FastFormat | 20 files, dirty | 1,044 | 1,067 | 1,034 |
| `dotnet format` | 1 file, formatted | 2,540 | 1,945 | 1,922 |
| `dotnet format` | 1 file, dirty | 2,077 | 1,985 | 2,013 |
| `dotnet format` | 20 files, formatted | 2,223 | 2,272 | 2,362 |
| `dotnet format` | 20 files, dirty | 2,395 | 2,610 | 2,398 |
| ReSharper CLI | 1 file, formatted | 10,348 | 10,260 | 10,227 |
| ReSharper CLI | 1 file, dirty | 10,325 | 10,264 | 10,504 |
| ReSharper CLI | 20 files, formatted | 15,827 | 16,840 | 16,657 |
| ReSharper CLI | 20 files, dirty | 17,272 | 17,600 | 18,058 |

## The no-op property

The important guarantee is that formatting already-formatted code does
nothing. Every byte the formatter emits is a whitespace decision derived from
the target profile, so on a file that is already in that profile the decision
is "leave it alone". That is what makes `format` safe to run over a whole
solution and keeps diffs meaningful. `format_source` enforces it by iterating to
a fixed point and reverting to the input if it cannot converge.

Measured on the 20 formatted copies: **0 of 20 files change** for
`csharp-format`, CSharpier changes all 20, and the other three change none.

Regression guard: `fixtures/noop_layout.cs` is a ReSharper-formatted file
kept as a golden test, so a change that starts rewriting formatted code fails
`cargo test` instead of silently costing a second pass and a dirty diff.

## What the tools actually produced

Speed only matters if the output is right. Reference: the original file, and
ReSharper's own output on the dirty input (which differs from the original in
160 lines — a stripped line has no authored column left to preserve).

| Tool | Formatted input rewritten (of 20) | Dirty output vs ReSharper's dirty output (differing lines) | Dirty output vs the reference (differing lines) |
| --- | ---: | ---: | ---: |
| **csharp-format** | **0** | **404** | 566 |
| CSharpier | 20 | 455 | 285 |
| FastFormat | 0 | 414 | 254 |
| `dotnet format` | 0 | 414 | 254 |
| ReSharper CLI | 0 | 0 | 160 |

* `csharp-format` reproduces the reference file **byte for byte** (BOM and line
  endings included) — the property the formatter is built around.
* `dotnet format` and FastFormat also leave the reference alone. Their outputs
  on the dirty input are **byte-identical to each other**: FastFormat is a
  faster entry point to the same Roslyn whitespace rules. Their remaining
  differences from the reference are style decisions where Roslyn's defaults
  differ from ReSharper's (for example Roslyn indents continuation lines by
  one level instead of aligning them under the opening parenthesis).
* ReSharper restores the file almost exactly (160 differing lines), as
  expected — it produced the reference in the first place.
* CSharpier is an opinionated pretty-printer, not a whitespace-preserving
  formatter. It reprints to its own style (breaks expression bodies and long
  tuples, moves the base-list colon), so its output is not comparable with the
  reference, and it adds lines to the file. It is not a drop-in replacement for
  ReSharper's profile.
* On the dirty input `csharp-format` is the closest tool to **ReSharper's**
  result (404 differing lines). Its distance from the
  *original* reference (566) is larger only because it reproduces ReSharper's
  rules faithfully: with `stick_comment` on, a comment that the stripping put
  at column zero stays at column zero, whereas the reference has it indented.
  FastFormat and `dotnet format` indent it, which scores better against the
  reference and worse against ReSharper. The dirty scenario is adversarial for
  a formatter that keeps authored continuation columns; read it as an upper
  bound, and compare it to ReSharper's column rather than the reference's.

### Caveats

* These are end-to-end command timings on one machine, not kernel
  microbenchmarks. Startup dominates the single-file numbers: CSharpier spends
  roughly 400 ms of its 451 ms before it reads the file; `dotnet format` spends
  ~1.8 s loading the workspace; ReSharper spends several seconds loading the
  solution and its engine.
* The tools do different amounts of work. `csharp-format` is a syntax-only
  single pass; `dotnet format` and ReSharper build a semantic model.
* Line endings were kept as-is for the timed runs. The comparison of outputs
  strips CR and the BOM so that files compare line by line.

## Reproduce

```sh
# working copy outside the repository
mkdir bench && cd bench
cp /path/to/RacePhase.cs .
cp /path/to/.editorconfig .

/workspace/csharp-format/target/release/csharp-format format .
csharpier format .
dotnet-fastformat --force .

# Roslyn formatter needs a project
dotnet format whitespace Bench.csproj --no-restore --verbosity quiet

# ReSharper needs a solution (see jbtmp/)
(cd jbtmp && dotnet jb cleanupcode t.sln --no-build --profile="Built-in: Reformat Code")
```
