# Collatz Benchmark

**Focus:** Integer loop/branch codegen. Sum of Collatz total stopping times for seeds 1..1,000,000 — a hot integer loop with a data-dependent branch and a function call per seed, complementing fibonacci's deep-recursion call profile with a flat iterative one.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Rust** | 103.7 ms ± 0.3 ms | 1.44 MB |
| **Swift** | 166.8 ms ± 5.5 ms | 5.55 MB |
| **Ryo (AOT)** | 219.3 ms ± 5.6 ms | 1.34 MB |
| **Ryo (JIT)** | 335.5 ms ± 3.7 ms | 4.88 MB |

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
