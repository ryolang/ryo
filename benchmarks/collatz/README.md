# Collatz Benchmark

**Focus:** Integer loop/branch codegen. Sum of Collatz total stopping times for seeds 1..1,000,000 — a hot integer loop with a data-dependent branch and a function call per seed, complementing fibonacci's deep-recursion call profile with a flat iterative one.

**Languages compared:** Ryo only (AOT vs JIT). Cross-language comparators are added per-benchmark when a phase needs that measurement.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-25. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Ryo (AOT)** | 215.2 ms ± 2.6 ms | 1.36 MB |
| **Ryo (JIT)** | 331.8 ms ± 5.1 ms | 4.77 MB |

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
