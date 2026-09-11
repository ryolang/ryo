# Collatz Benchmark

**Focus:** Integer loop/branch codegen. Sum of Collatz total stopping times for seeds 1..1,000,000 — a hot integer loop with a data-dependent branch and a function call per seed, complementing fibonacci's deep-recursion call profile with a flat iterative one.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 104.3 ms ± 1.2 ms | 1.00x | 1.44 MB |
| **Swift** | 6.3.3 | 164.4 ms ± 1.0 ms | 1.58x slower | 5.55 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+b3b7d25 | 216.0 ms ± 1.3 ms | 2.07x slower | 1.36 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+b3b7d25 | 220.8 ms ± 2.3 ms | 2.12x slower | 5.09 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
