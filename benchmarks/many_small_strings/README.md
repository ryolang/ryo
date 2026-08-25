# Many Small Strings Benchmark

**Focus:** Flat-loop alloc/free churn. Builds 500,000 short strings (`int_to_str(i) + "!"`), keeps none — each iteration's buffer is freed at its last use inside the same iteration. Complements `eager_destruction` (which stresses deep-recursion lifetimes) with high-frequency flat-loop churn through `ryo_int_to_str`, `ryo_str_concat`, and `ryo_str_free`.

**Languages compared:** Ryo only (AOT vs JIT). Cross-language comparators are added per-benchmark when a phase needs that measurement.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-25 (post-ABI). Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux). Pre-ABI rows are the same-day checkpoint recorded before commit `7d0a047`.

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Ryo (AOT)** | 19.7 ms ± 0.4 ms | 1.39 MB |
| **Ryo (JIT)** | 21.8 ms ± 0.8 ms | 4.66 MB |
| Ryo (AOT), pre-ABI | 19.0 ms ± 0.4 ms | 1.39 MB |
| Ryo (JIT), pre-ABI | 20.9 ms ± 0.8 ms | 4.62 MB |

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
