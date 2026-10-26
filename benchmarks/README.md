# Ryo Language Benchmarks

This directory contains various benchmarks used to measure, validate, and compare the performance and memory footprint of the **Ryo** programming language compiler and runtime against other languages.

**Ryo's performance goal:** beat **Python** on performance and **Rust** on simplicity/ergonomics. These suites therefore serve as *regression guardrails*, not parity targets: Ryo already runs fib(40) roughly **19× faster than Python** with the lightest memory footprint of all languages tested. Chasing the remaining gap to Rust/Swift instruction-by-instruction was tried (Cranelift upgrade, string runtime ABI redesign, value-range guard elision, 2026-08) and produced ~zero walltime change — out-of-order hardware absorbs those wins, and the residual gap is structural (middle-end transforms), so micro-optimization is parked until the language is feature-complete. The durable findings live in [`docs/dev/cranelift_lessons.md`](../docs/dev/cranelift_lessons.md).

We target both execution speed and memory efficiency (specifically focusing on Ryo's Ahead-Of-Time AOT compiler via Cranelift and JIT execution).

**Idiomatic convention:** every benchmark is written the way a developer would naturally write it in that language. Never adapt another language's implementation to work around something Ryo doesn't support yet (e.g. moving a field out of a struct) — the cost of Ryo's current limitations is part of what the suite measures. Where that makes workloads diverge, checksums are per-language and the benchmark's README must say so explicitly.

**Checkpoint convention:** run the full suite before each release and after merging any change that touches generated-code shape (`ryo-backend/src/codegen/`, the Cranelift pin, ownership sidecar consumption); results land in the per-suite data store and the README tables are regenerated from it, so the trend is visible in git history. Every per-suite results table must include a **Version** column capturing each language toolchain's version at measurement time (`rustc --version`, `swiftc --version`, `python3 --version`, `ryo --version`) — timings without versions are not reproducible. The root matrix omits versions; its detailed per-suite tables retain them.

---

## Benchmark Matrix

Per-language matrix across suites — one row per suite, one column per language — regenerated from the data store by `uv run benchmarks/run.py update-readme`; do not edit the tables by hand. Library-comparison arms fold into their language's column (fastest wins); per-library detail lives in the suite READMEs.

<!-- BENCH-MATRIX:START -->
⭐ fastest in row · 🟩 within 20% · 🟨 within 3× · 🟥 beyond 3×
**Runtime (mean)**
| Suite | Ryo AOT | Ryo JIT | Rust | Swift | Go | Python | Kotlin | Bun (TS) | Elixir | Ruby | Julia |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Algorithms** |  |  |  |  |  |  |  |  |  |  |  |
| [collatz](./collatz/)<br>2026-10-22 | 🟨 234.233 [2.08×] | 🟨 233.308 [2.07×] | ⭐ 112.637 [1.00×] | 🟨 175.254 [1.56×] | — | — | — | — | — | — | — |
| [fibonacci](./fibonacci/)<br>2026-10-22 | 🟨 375.421 [1.35×] | 🟨 403.351 [1.45×] | ⭐ 278.001 [1.00×] | 🟨 357.573 [1.29×] | 🟩 306.954 [1.10×] | 🟥 5388.1 [19.4×] | 🟩 296.2 [1.07×] | 🟨 421.819 [1.52×] | 🟥 958.776 [3.45×] | 🟥 6427.91 [23.1×] | 🟨 437.615 [1.57×] |
| [mandelbrot](./mandelbrot/)<br>2026-10-22 | 🟩 15.45 [1.14×] | 🟨 17.133 [1.26×] | ⭐ 13.56 [1.00×] | 🟩 14.587 [1.08×] | — | — | — | — | — | — | — |
| **Lifecycle** |  |  |  |  |  |  |  |  |  |  |  |
| [eager_destruction](./eager_destruction/)<br>2026-10-22 | ⭐ 2.063 [1.00×] | 🟨 3.091 [1.50×] | 🟨 3.74 [1.81×] | — | — | — | — | — | — | — | — |
| **Parsing** |  |  |  |  |  |  |  |  |  |  |  |
| [json_validate](./json_validate/)<br>2026-10-22 | 🟥 92.4 [17.1×] | 🟥 118.102 [21.9×] | ⭐ 5.395 [1.00×] | 🟥 33.704 [6.25×] | 🟥 41.326 [7.66×] | 🟥 2197.2 [407×] | — | — | — | — | — |
| **Strings** |  |  |  |  |  |  |  |  |  |  |  |
| [byte_slicing](./byte_slicing/)<br>2026-10-22 | 🟨 3.409 [1.91×] | 🟨 5.143 [2.88×] | ⭐ 1.787 [1.00×] | 🟨 3.372 [1.89×] | — | — | — | — | — | — | — |
| [doubling_concat](./doubling_concat/)<br>2026-10-22 | 🟨 6.379 [1.38×] | 🟨 5.995 [1.30×] | ⭐ 4.608 [1.00×] | 🟩 5.302 [1.15×] | — | — | — | — | — | — | — |
| [many_small_strings](./many_small_strings/)<br>2026-10-22 | ⭐ 8.497 [1.00×] | 🟩 10.134 [1.19×] | 🟨 10.559 [1.24×] | 🟨 10.598 [1.25×] | — | — | — | — | — | — | — |
| [string_building](./string_building/)<br>2026-10-22 | 🟩 1.573 [1.09×] | 🟨 2.833 [1.97×] | ⭐ 1.44 [1.00×] | 🟨 2.314 [1.61×] | — | 🟥 36.72 [25.5×] | — | — | — | — | — |
| [string_slicing](./string_slicing/)<br>2026-10-22 | 🟨 3.493 [1.82×] | 🟨 4.992 [2.60×] | ⭐ 1.919 [1.00×] | 🟥 18.499 [9.64×] | — | — | — | — | — | — | — |
| **Structs** |  |  |  |  |  |  |  |  |  |  |  |
| [struct_records](./struct_records/)<br>2026-10-22 | ⭐ 10.533 [1.00×] | 🟩 12.294 [1.17×] | 🟨 24.479 [2.32×] | 🟩 11.896 [1.13×] | 🟨 25.911 [2.46×] | 🟥 113.693 [10.8×] | — | — | — | — | — |
| [struct_records_inout](./struct_records_inout/)<br>2026-10-22 | ⭐ 10.941 [1.00×] | 🟩 12.431 [1.14×] | 🟨 24.468 [2.24×] | 🟩 12.322 [1.13×] | 🟨 26.265 [2.40×] | 🟥 95.276 [8.71×] | — | — | — | — | — |
| [struct_records_reuse](./struct_records_reuse/)<br>2026-10-22 | 🟨 14.626 [1.21×] | 🟨 15.783 [1.31×] | 🟨 32.063 [2.66×] | ⭐ 12.072 [1.00×] | 🟨 26.167 [2.17×] | 🟥 123.295 [10.2×] | — | — | — | — | — |
| **Summary (geomean)** | 🟨 1.57× | 🟨 1.92× | 🟨 1.30× | 🟨 1.73× | 🟨 2.55× | 🟥 24.0× | ⭐ 1.07× | 🟨 1.52× | 🟥 3.45× | 🟥 23.1× | 🟨 1.57× |

**Peak memory (max RSS)**
| Suite | Ryo AOT | Ryo JIT | Rust | Swift | Go | Python | Kotlin | Bun (TS) | Elixir | Ruby | Julia |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Algorithms** |  |  |  |  |  |  |  |  |  |  |  |
| [collatz](./collatz/)<br>2026-10-22 | ⭐ 1.369 [1.00×] | 🟥 5.722 [4.18×] | 🟩 1.492 [1.09×] | 🟥 5.576 [4.07×] | — | — | — | — | — | — | — |
| [fibonacci](./fibonacci/)<br>2026-10-22 | ⭐ 1.352 [1.00×] | 🟥 5.478 [4.05×] | 🟩 1.478 [1.09×] | 🟩 1.609 [1.19×] | 🟨 3.895 [2.88×] | 🟥 17.206 [12.7×] | 🟥 44.669 [33.0×] | 🟥 27.428 [20.3×] | 🟥 90.241 [66.7×] | 🟥 27.997 [20.7×] | 🟥 193.627 [143×] |
| [mandelbrot](./mandelbrot/)<br>2026-10-22 | ⭐ 1.37 [1.00×] | 🟥 5.833 [4.26×] | 🟩 1.493 [1.09×] | 🟥 5.577 [4.07×] | — | — | — | — | — | — | — |
| **Lifecycle** |  |  |  |  |  |  |  |  |  |  |  |
| [eager_destruction](./eager_destruction/)<br>2026-10-22 | ⭐ 1.369 [1.00×] | 🟥 5.505 [4.02×] | 🟥 6.813 [4.98×] | — | — | — | — | — | — | — | — |
| **Parsing** |  |  |  |  |  |  |  |  |  |  |  |
| [json_validate](./json_validate/)<br>2026-10-22 | ⭐ 5.698 [1.00×] | 🟨 12.466 [2.19×] | 🟩 5.895 [1.03×] | 🟨 10.407 [1.83×] | 🟨 13.915 [2.44×] | 🟥 28.012 [4.92×] | — | — | — | — | — |
| **Strings** |  |  |  |  |  |  |  |  |  |  |  |
| [byte_slicing](./byte_slicing/)<br>2026-10-22 | ⭐ 2.76 [1.00×] | 🟨 7.6 [2.75×] | 🟩 2.908 [1.05×] | 🟨 7.11 [2.58×] | — | — | — | — | — | — | — |
| [doubling_concat](./doubling_concat/)<br>2026-10-22 | ⭐ 33.417 [1.00×] | 🟩 37.685 [1.13×] | 🟩 35.689 [1.07×] | 🟩 34.078 [1.02×] | — | — | — | — | — | — | — |
| [many_small_strings](./many_small_strings/)<br>2026-10-22 | ⭐ 1.37 [1.00×] | 🟥 5.57 [4.07×] | 🟩 1.537 [1.12×] | 🟩 1.609 [1.17×] | — | — | — | — | — | — | — |
| [string_building](./string_building/)<br>2026-10-22 | ⭐ 1.494 [1.00×] | 🟥 5.698 [3.81×] | 🟩 1.65 [1.10×] | 🟨 1.859 [1.24×] | — | 🟥 17.602 [11.8×] | — | — | — | — | — |
| [string_slicing](./string_slicing/)<br>2026-10-22 | ⭐ 2.756 [1.00×] | 🟨 7.699 [2.79×] | 🟩 2.916 [1.06×] | 🟨 7.045 [2.56×] | — | — | — | — | — | — | — |
| **Structs** |  |  |  |  |  |  |  |  |  |  |  |
| [struct_records](./struct_records/)<br>2026-10-22 | ⭐ 1.354 [1.00×] | 🟥 6.012 [4.44×] | 🟩 1.542 [1.14×] | 🟩 1.609 [1.19×] | 🟥 9.312 [6.88×] | 🟥 17.287 [12.8×] | — | — | — | — | — |
| [struct_records_inout](./struct_records_inout/)<br>2026-10-22 | ⭐ 1.354 [1.00×] | 🟥 5.955 [4.40×] | 🟩 1.546 [1.14×] | 🟩 1.61 [1.19×] | 🟥 9.354 [6.91×] | 🟥 17.294 [12.8×] | — | — | — | — | — |
| [struct_records_reuse](./struct_records_reuse/)<br>2026-10-22 | ⭐ 1.37 [1.00×] | 🟥 6.041 [4.41×] | 🟩 1.589 [1.16×] | 🟩 1.625 [1.19×] | 🟥 9.337 [6.82×] | 🟥 17.29 [12.6×] | — | — | — | — | — |
| **Summary (geomean)** | ⭐ 1.00× | 🟥 3.37× | 🟨 1.23× | 🟨 1.70× | 🟥 4.69× | 🟥 10.7× | 🟥 33.0× | 🟥 20.3× | 🟥 66.7× | 🟥 20.7× | 🟥 143× |

*Cells show each suite's best arm per language — mean time or peak RSS, ratio vs. the fastest arm in the row; the summary row is a per-column geomean of those ratios.*
<!-- BENCH-MATRIX:END -->

---

## General Prerequisites

To run these benchmarks you need `hyperfine`, the per-language toolchains the selected suites compare (each suite README lists its arms), and `uv`. Every suite is driven from the repository root by the central runner `benchmarks/run.py`, which discovers each suite's `suite.toml` manifest — there are no per-suite scripts:

```bash
# Time every arm of a suite (drop --benchmarks to run every discovered suite)
uv run benchmarks/run.py run --benchmarks eager_destruction

# Build-only correctness gate: build every arm and run its checksum, no hyperfine
uv run benchmarks/run.py run --benchmarks eager_destruction --dry-run
```

### Runner Contract

The runner's behavior is fixed by the manifests and the data store, not by the per-suite wrapper-script copies this runner replaced:

* **Subcommands.** `run` builds each arm, verifies its checksum output, then times it with `hyperfine` (peak RSS from hyperfine 2.0's native memory measurement, reported as the mean of runs) and appends the numbers to the suite's store under `benchmarks/data/`. `table` renders the canonical tables from the store to stdout. `update-readme` splices those tables into the README marker blocks (see the checkpoint convention below); suites with no store data yet are left untouched, so a bare `update-readme` on a fresh clone changes nothing. `matrix` renders the cross-suite per-language matrix (the `## Benchmark Matrix` section below) from the store.
* **Flags.** `run` takes `--benchmarks NAMES`, `--languages NAMES`, `--dry-run` (stop after the checksum gate; never invoke hyperfine), `--strict` (fail before benchmarking when a selected toolchain is missing), `--ryo-bin PATH`, and `--json PATH` (write the incremental run JSON after each suite). `table` and `update-readme` take `--benchmarks` (and `table` takes `--out PATH`). Both also take `--include-ignored` to act on git-ignored (local) suites — see below.
* **Committed vs. local suites.** Discovery is git-aware: `table` and `update-readme` skip manifests git marks ignored (the local `csv_parse`, `json_parse`, and `startup_latency` sweeps), so committed READMEs and the matrix only ever carry committed suites. `run` always discovers everything — local sweeps stay a legitimate target. To render or splice a local suite, pass `--include-ignored`.
* **Ryo binary precedence.** `--ryo-bin PATH` wins, then `$RYO_BIN`, then the repo's own `target/release/ryo`. Only that last default is ever built by the runner (`cargo build --release`, pinned to the repo root no matter where the runner is invoked); an explicit override must already exist.
* **Store keys and retention.** The store is keyed `<suite>/<group>/<arm>` (the group segment appears only in suites that define groups) and keeps the newest 20 entries per key. Entries come only from real `run` invocations — never backfilled or hand-edited — so every rendered row traces to an actual measurement with its date and commit.
* **Ryo (AOT) arms chain the compiler build.** Each suite's Ryo (AOT) arm begins its manifest build line with `cargo build --release`, so the benchmarked binary always comes from the just-built compiler at the measured revision. That chain is deliberate — it stays faithful to the old per-suite scripts even where it duplicates the runner's own auto-build.

---

## Profiling with Samply

To deeply inspect the performance characteristics of Ryo's execution via flamegraphs, we recommend using [samply](https://github.com/mstange/samply).

First, install `samply`:

```bash
cargo install samply
```

Then, navigate to any benchmark directory, build the benchmark, and profile either the JIT execution or the standalone AOT compiled binary.

### Example: Profiling Fibonacci

**Profile the standalone AOT binary:**

```bash
samply record ./fib
```

**Profile the JIT compiler executing a file:**

```bash
samply record ../../target/release/ryo run fib.ryo
```

`samply` will execute the provided command and automatically open a browser window displaying an interactive flamegraph, allowing you to trace Cranelift codegen overhead versus actual application execution time.

---

## Manual Checkpoint Convention

Run the full suite **before each release** and **after merging any change that touches generated-code shape** (`ryo-backend/src/codegen/`, the Cranelift pin, ownership sidecar consumption):

```bash
uv run benchmarks/run.py run            # every discovered suite
uv run benchmarks/run.py update-readme  # splice the new tables into the READMEs
```

The numbers land in the per-suite store (`benchmarks/data/<suite>.json`); `update-readme` renders the canonical table into each suite README's `<!-- BENCH:START -->` / `<!-- BENCH:END -->` block and the per-language matrix into this README's `<!-- BENCH-MATRIX:START -->` / `<!-- BENCH-MATRIX:END -->` block. Never edit a generated block by hand and never copy numbers into it — a rendered row exists only because a real `run` measured it. Hand-written tables survive only as dated checkpoint history below the generated block in each suite README, and prose that cites specific numbers (e.g. "1.7 ms") goes stale when the table moves — update-readme warns about those. These runs are manual only — they never run in CI.
