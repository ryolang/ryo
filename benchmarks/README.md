# Ryo Language Benchmarks

This directory contains various benchmarks used to measure, validate, and compare the performance and memory footprint of the **Ryo** programming language compiler and runtime against other languages.

**Ryo's performance goal:** beat **Python** on performance and **Rust** on simplicity/ergonomics. These suites therefore serve as *regression guardrails*, not parity targets: Ryo already runs fib(40) roughly **19× faster than Python** with the lightest memory footprint of all languages tested. Chasing the remaining gap to Rust/Swift instruction-by-instruction was tried (Cranelift upgrade, string runtime ABI redesign, value-range guard elision, 2026-08) and produced ~zero walltime change — out-of-order hardware absorbs those wins, and the residual gap is structural (middle-end transforms), so micro-optimization is parked until the language is feature-complete. The durable findings live in [`docs/dev/cranelift_lessons.md`](../docs/dev/cranelift_lessons.md).

We target both execution speed and memory efficiency (specifically focusing on Ryo's Ahead-Of-Time AOT compiler via Cranelift and JIT execution).

**Idiomatic convention:** every benchmark is written the way a developer would naturally write it in that language. Never adapt another language's implementation to work around something Ryo doesn't support yet (e.g. moving a field out of a struct) — the cost of Ryo's current limitations is part of what the suite measures. Where that makes workloads diverge, checksums are per-language and the benchmark's README must say so explicitly.

**Checkpoint convention:** run the full suite before each release and after merging any change that touches generated-code shape (`ryo-backend/src/codegen/`, the Cranelift pin, ownership sidecar consumption); results land in the per-suite data store and the README tables are regenerated from it, so the trend is visible in git history. Every results table must include a **Version** column capturing each language toolchain's version at measurement time (`rustc --version`, `swiftc --version`, `python3 --version`, `ryo --version`) — timings without versions are not reproducible.

---

## Current Results

Cross-suite summary of the Ryo (AOT) anchor row per suite, regenerated from the data store by `uv run benchmarks/run.py update-readme` — do not edit the table by hand (see the checkpoint convention below). Each row links the suite and lists its arms, then shows the Ryo-AOT time and peak RSS, the same-run ratios against the fastest and leanest arms (`—` when no same-run comparison exists), and the toolchains measured in that run.

<!-- BENCH-SUMMARY:START -->
| Suite | Languages | Ryo AOT | Peak RSS | vs. fastest | vs. leanest | Version | Measured |
|---|---|---|---|---|---|---|---|
| [collatz](./collatz/) | Rust, Swift, Ryo (AOT), Ryo (JIT) | 215.607 ms | 1.368 MB | 2.07x | 1.00x | ryo 0.1.0-dev.20261022+280990f / rustc 1.99.1 / swiftc 6.4 | 2026-10-21 (commit 280990f) |
| [fibonacci](./fibonacci/) | Rust, Kotlin, Go, Swift, Ryo (AOT), Ryo (JIT), Bun (TS), Elixir, Ruby, Python, Julia | 361.187 ms | 1.35 MB | 1.37x | 1.00x | ryo 0.1.0-dev.20261022+63c9cba / rustc 1.99.1 / kotlinc 2.4.21 / go 1.27.3 / swiftc 6.4 / bun 1.3.13 / elixir 1.20.4 / ruby 2.6.10p210 / python 3.14.4 / julia 1.13.2 | 2026-10-21 (commit 63c9cba) |
<!-- BENCH-SUMMARY:END -->

---

## Benchmark Matrix

Per-language matrix across suites — one row per suite, one column per language — regenerated from the data store by `uv run benchmarks/run.py update-readme`; do not edit the tables by hand. Library-comparison arms fold into their language's column (fastest wins); per-library detail lives in the suite READMEs.

<!-- BENCH-MATRIX:START -->
⭐ fastest in row · 🟩 within 20% · 🟨 within 3× · 🟥 beyond 3×
**Runtime (mean)**
| Suite | Ryo AOT | Ryo JIT | Rust | Swift | Go | Python | Kotlin | Bun (TS) | Elixir | Ruby | Julia |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Algorithms** |  |  |  |  |  |  |  |  |  |  |  |
| [collatz](./collatz/)<br>2026-10-21 | 🟨 215.607 [2.07×] (0.1.0-dev.20261022+280990f) | 🟨 215.136 [2.06×] (0.1.0-dev.20261022+280990f) | ⭐ 104.392 [1.00×] (1.99.1) | 🟨 163.709 [1.57×] (6.4) | — | — | — | — | — | — | — |
| [fibonacci](./fibonacci/)<br>2026-10-21 | 🟨 361.187 [1.37×] (0.1.0-dev.20261022+63c9cba) | 🟨 383.871 [1.46×] (0.1.0-dev.20261022+63c9cba) | ⭐ 263.58 [1.00×] (1.99.1) | 🟨 329.675 [1.25×] (6.4) | 🟩 295.1 [1.12×] (1.27.3) | 🟥 4997.22 [19.0×] (3.14.4) | 🟩 269.675 [1.02×] (2.4.21) | 🟨 402.576 [1.53×] (1.3.13) | 🟥 916.317 [3.48×] (1.20.4) | 🟥 6252.58 [23.7×] (2.6.10p210) | 🟨 415.231 [1.58×] (1.13.2) |
| **Summary (geomean)** | 🟨 1.68× | 🟨 1.73× | ⭐ 1.00× | 🟨 1.40× | 🟩 1.12× | 🟥 19.0× | 🟩 1.02× | 🟨 1.53× | 🟥 3.48× | 🟥 23.7× | 🟨 1.58× |

**Peak memory (max RSS)**
| Suite | Ryo AOT | Ryo JIT | Rust | Swift | Go | Python | Kotlin | Bun (TS) | Elixir | Ruby | Julia |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Algorithms** |  |  |  |  |  |  |  |  |  |  |  |
| [collatz](./collatz/)<br>2026-10-21 | ⭐ 1.368 [1.00×] (0.1.0-dev.20261022+280990f) | 🟥 5.74 [4.20×] (0.1.0-dev.20261022+280990f) | 🟩 1.495 [1.09×] (1.99.1) | 🟥 5.577 [4.08×] (6.4) | — | — | — | — | — | — | — |
| [fibonacci](./fibonacci/)<br>2026-10-21 | ⭐ 1.35 [1.00×] (0.1.0-dev.20261022+63c9cba) | 🟥 5.514 [4.08×] (0.1.0-dev.20261022+63c9cba) | 🟩 1.482 [1.10×] (1.99.1) | 🟩 1.609 [1.19×] (6.4) | 🟨 3.881 [2.87×] (1.27.3) | 🟥 15.983 [11.8×] (3.14.4) | 🟥 44.633 [33.1×] (2.4.21) | 🟥 27.438 [20.3×] (1.3.13) | 🟥 90.094 [66.7×] (1.20.4) | 🟥 28 [20.7×] (2.6.10p210) | 🟥 193.602 [143×] (1.13.2) |
| **Summary (geomean)** | ⭐ 1.00× | 🟥 4.14× | 🟩 1.10× | 🟨 2.20× | 🟨 2.87× | 🟥 11.8× | 🟥 33.1× | 🟥 20.3× | 🟥 66.7× | 🟥 20.7× | 🟥 143× |

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
* **Committed vs. local suites.** Discovery is git-aware: `table` and `update-readme` skip manifests git marks ignored (the local `csv_parse`, `json_parse`, and `startup_latency` sweeps), so committed READMEs and the summary only ever carry committed suites. `run` always discovers everything — local sweeps stay a legitimate target. To render or splice a local suite, pass `--include-ignored`.
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

The numbers land in the per-suite store (`benchmarks/data/<suite>.json`); `update-readme` renders the canonical table into each suite README's `<!-- BENCH:START -->` / `<!-- BENCH:END -->` block and the cross-suite summary into this README's `<!-- BENCH-SUMMARY:START -->` / `<!-- BENCH-SUMMARY:END -->` block. Never edit a generated block by hand and never copy numbers into it — a rendered row exists only because a real `run` measured it. Hand-written tables survive only as dated checkpoint history below the generated block in each suite README, and prose that cites specific numbers (e.g. "1.7 ms") goes stale when the table moves — update-readme warns about those. These runs are manual only — they never run in CI.
