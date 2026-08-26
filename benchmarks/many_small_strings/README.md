# Many Small Strings Benchmark

**Focus:** Flat-loop alloc/free churn. Builds 500,000 short strings (`int_to_str(i) + "!"`), keeps none — each iteration's buffer is freed at its last use inside the same iteration. Complements `eager_destruction` (which stresses deep-recursion lifetimes) with high-frequency flat-loop churn through `ryo_int_to_str`, `ryo_str_concat`, and `ryo_str_free`.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Rust** | 9.8 ms ± 0.2 ms | 1.50 MB |
| **Swift** | 10.1 ms ± 0.4 ms | 1.58 MB |
| **Ryo (AOT)** | 19.1 ms ± 0.8 ms | 1.39 MB |
| **Ryo (JIT)** | 21.0 ms ± 0.5 ms | 4.70 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
