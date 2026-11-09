# Benchmark

Wall-clock comparison of `csharp-format` against the formatters it is normally
compared with: `dotnet format whitespace`, [FastFormat], [CSharpier] and the
ReSharper command-line tool.

The headline: on a 871-line file `csharp-format` takes **80 ms**; the next
fastest tool takes **0.49 s** and the slowest **10.3 s**. Most of the gap is
process startup — our formatter is a native binary, the others start a .NET
runtime (and, for `dotnet format` and ReSharper, load an MSBuild workspace).

[FastFormat]: https://github.com/lyssieth/dotnet-fastformat
[CSharpier]: https://csharpier.com/

## Environment

| | |
| --- | --- |
| OS | Windows 11 25H2 (build 28000), Git Bash |
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

`RacePhase.cs` from the NFM-World game, an 871-line / 33,264-byte file that was
produced by ReSharper and is treated as the reference output. The project's
`.editorconfig` sits next to every copy of the file.

* **formatted** — the file as-is.
* **dirty** — the same file with every line's leading spaces and tabs removed
  (871 lines, 26,020 bytes). Line breaks, comments and tokens are unchanged.
* **20 files** — 20 copies of the file (17,420 lines, ~665 KB).

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
| **csharp-format 0.1.0** | **80** | **89** | **117** | **157** |
| CSharpier 1.3.0 | 490 | 484 | 666 | 678 |
| FastFormat 1.5.0 | 591 | 648 | 912 | 1,039 |
| `dotnet format whitespace` | 1,905 | 1,907 | 2,086 | 2,186 |
| ReSharper CLI | 10,274 | 11,419 | 16,140 | 17,342 |

Relative to `csharp-format` on the formatted inputs: CSharpier 6.1×, FastFormat
7.4×, `dotnet format` 23.8×, ReSharper 128× (1 file); 5.7×, 7.8×, 17.8× and
138× (20 files).

Per file on the 20-file dirty input: `csharp-format` **7.9 ms**, CSharpier
34 ms, FastFormat 52 ms, `dotnet format` 109 ms, ReSharper 867 ms. The marginal
cost of each additional file (formatted input) is about **1.9 ms** for
`csharp-format`, versus 9.3 ms (CSharpier), 9.5 ms (`dotnet format`), 17 ms
(FastFormat) and 309 ms (ReSharper).

### All runs

| Tool | Cell | Run 1 | Run 2 | Run 3 |
| --- | --- | ---: | ---: | ---: |
| csharp-format | 1 file, formatted | 83 | 83 | 80 |
| csharp-format | 1 file, dirty | 98 | 94 | 89 |
| csharp-format | 20 files, formatted | 123 | 117 | 121 |
| csharp-format | 20 files, dirty | 159 | 157 | 157 |
| CSharpier | 1 file, formatted | 490 | 491 | 497 |
| CSharpier | 1 file, dirty | 484 | 489 | 510 |
| CSharpier | 20 files, formatted | 666 | 716 | 753 |
| CSharpier | 20 files, dirty | 678 | 678 | 678 |
| FastFormat | 1 file, formatted | 670 | 591 | 594 |
| FastFormat | 1 file, dirty | 671 | 663 | 648 |
| FastFormat | 20 files, formatted | 912 | 940 | 916 |
| FastFormat | 20 files, dirty | 1,070 | 1,049 | 1,039 |
| `dotnet format` | 1 file, formatted | 1,983 | 1,905 | 1,928 |
| `dotnet format` | 1 file, dirty | 1,989 | 1,907 | 1,988 |
| `dotnet format` | 20 files, formatted | 2,204 | 2,110 | 2,086 |
| `dotnet format` | 20 files, dirty | 2,309 | 2,339 | 2,186 |
| ReSharper CLI | 1 file, formatted | 10,599 | 10,274 | 11,413 |
| ReSharper CLI | 1 file, dirty | 11,419 | 11,813 | 11,849 |
| ReSharper CLI | 20 files, formatted | 16,140 | 16,254 | 16,258 |
| ReSharper CLI | 20 files, dirty | 17,342 | 17,824 | 18,766 |

## What the tools actually produced

Speed only matters if the output is right. On the same input:

| Tool | Formatted input rewritten (of 20) | Dirty input rewritten (of 20) | Dirty result vs reference (differing lines) |
| --- | ---: | ---: | ---: |
| **csharp-format** | **0** | 20 | 324 |
| CSharpier | 20 | 20 | 247 (but 940 lines) |
| FastFormat | 0 | 20 | 246 |
| `dotnet format` | 0 | 20 | 246 |
| ReSharper CLI | 0 | 20 | 112 |

* `csharp-format` reproduces the reference file **byte for byte** (CRLF and
  BOM included) — the property the formatter is built around.
* `dotnet format` and FastFormat also leave the reference alone. Their outputs
  on the dirty input are **byte-identical to each other**: FastFormat is a
  faster entry point to the same Roslyn whitespace rules. Their remaining
  differences from the reference are style decisions where Roslyn's defaults
  differ from ReSharper's (for example Roslyn indents continuation lines by
  one level instead of aligning them under the opening parenthesis).
* ReSharper restores the file almost exactly (112 differing lines), as
  expected — it produced the reference in the first place.
* CSharpier is an opinionated pretty-printer, not a whitespace-preserving
  formatter. It reprints to its own style (breaks expression bodies and long
  tuples, moves the base-list colon), so its output is not comparable with the
  reference, and it adds 69 lines to the file. It is not a drop-in replacement
  for ReSharper's profile.
* `csharp-format`'s 324 differing lines on the dirty input come from one
  deliberate rule: **continuation lines keep their source column**. On a file
  whose indentation was destroyed that means alignment is not recovered, only
  block/statement indentation. This rule is what makes the formatter a no-op on
  already-formatted code (0/20 rewritten above); changing it would trade that
  property away. The dirty scenario is adversarial for this design, and the
  numbers above should be read that way — it is not a defect against the
  formatted-input case.

### Caveats

* These are end-to-end command timings on one machine, not kernel
  microbenchmarks. Startup dominates the single-file numbers: CSharpier spends
  roughly 400 ms of its 490 ms before it reads the file; `dotnet format` spends
  ~1.8 s loading the workspace; ReSharper spends several seconds loading the
  solution and its engine.
* The tools do different amounts of work. `csharp-format` is a syntax-only
  single pass; `dotnet format` and ReSharper build a semantic model.
* Line endings were kept as-is for the timed runs. The comparison of outputs
  strips CR so that a CRLF reference and a CRLF result compare line by line.

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
