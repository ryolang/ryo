# Many Small Strings Benchmark

**Focus:** Flat-loop alloc/free churn. Builds 500,000 short strings (`int_to_str(i) + "!"`), keeps none — each iteration's buffer is freed at its last use inside the same iteration. Complements `eager_destruction` (which stresses deep-recursion lifetimes) with high-frequency flat-loop churn through `ryo_int_to_str`, `ryo_str_concat`, and `ryo_str_free`.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 10.3 ms ± 0.3 ms | 1.00x | 1.50 MB |
| **Swift** | 6.3.3 | 10.5 ms ± 0.5 ms | 1.03x slower | 1.58 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+b3b7d25 | 19.0 ms ± 0.5 ms | 1.86x slower | 1.38 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+b3b7d25 | 20.9 ms ± 0.7 ms | 2.04x slower | 4.88 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
