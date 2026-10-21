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
| [collatz](./collatz/) | Rust, Swift, Ryo (AOT), Ryo (JIT) | 239.027 ms | 1.352 MB | 2.14x | 1.00x | ryo 0.1.0 / rustc 1.98.0 / swiftc 6.4 | 2026-10-21 (commit bddd4e1) |
| [fibonacci](./fibonacci/) | Rust, Kotlin, Go, Swift, Ryo (AOT), Ryo (JIT), Bun (TS), Elixir, Ruby, Python, Julia | 359.464 ms | 1.37 MB | 1.34x | 1.00x | ryo 0.1.0 / rustc 1.98.0 / kotlinc 2.4.21 / go 1.27.2 / swiftc 6.4 / bun 1.3.13 / elixir 1.20.4 / ruby 2.6.10p210 / uv 3.14.4 / julia 1.13.1 | 2026-10-21 (commit bddd4e1) |
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
| [collatz](./collatz/)<br>2026-10-21 | 🟨 239.027 [2.14×] (0.1.0) | 🟨 255.785 [2.29×] (0.1.0) | ⭐ 111.626 [1.00×] (1.98.0) | 🟨 181.625 [1.63×] (6.4) | — | — | — | — | — | — | — |
| [fibonacci](./fibonacci/)<br>2026-10-21 | 🟨 359.464 [1.34×] (0.1.0) | 🟨 389.487 [1.46×] (0.1.0) | ⭐ 267.655 [1.00×] (1.98.0) | 🟨 329.754 [1.23×] (6.4) | 🟩 301.492 [1.13×] (1.27.2) | 🟥 5068.66 [18.9×] (3.14.4) | 🟩 280.338 [1.05×] (2.4.21) | 🟨 406.479 [1.52×] (1.3.13) | 🟥 990.888 [3.70×] (1.20.4) | 🟥 6313.35 [23.6×] (2.6.10p210) | 🟨 429.82 [1.61×] (1.13.1) |
| **Summary (geomean)** | 🟨 1.70× | 🟨 1.83× | ⭐ 1.00× | 🟨 1.42× | 🟩 1.13× | 🟥 18.9× | 🟩 1.05× | 🟨 1.52× | 🟥 3.70× | 🟥 23.6× | 🟨 1.61× |

**Peak memory (max RSS)**
| Suite | Ryo AOT | Ryo JIT | Rust | Swift | Go | Python | Kotlin | Bun (TS) | Elixir | Ruby | Julia |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Algorithms** |  |  |  |  |  |  |  |  |  |  |  |
| [collatz](./collatz/)<br>2026-10-21 | ⭐ 1.352 [1.00×] (0.1.0) | 🟥 5.704 [4.22×] (0.1.0) | 🟩 1.463 [1.08×] (1.98.0) | 🟥 5.576 [4.12×] (6.4) | — | — | — | — | — | — | — |
| [fibonacci](./fibonacci/)<br>2026-10-21 | ⭐ 1.37 [1.00×] (0.1.0) | 🟥 5.475 [4.00×] (0.1.0) | 🟩 1.476 [1.08×] (1.98.0) | 🟩 1.609 [1.17×] (6.4) | 🟨 3.942 [2.88×] (1.27.2) | 🟥 16 [11.7×] (3.14.4) | 🟥 44.647 [32.6×] (2.4.21) | 🟥 27.433 [20.0×] (1.3.13) | 🟥 89.569 [65.4×] (1.20.4) | 🟥 28 [20.4×] (2.6.10p210) | 🟥 193.684 [141×] (1.13.1) |
| **Summary (geomean)** | ⭐ 1.00× | 🟥 4.11× | 🟩 1.08× | 🟨 2.20× | 🟨 2.88× | 🟥 11.7× | 🟥 32.6× | 🟥 20.0× | 🟥 65.4× | 🟥 20.4× | 🟥 141× |

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
