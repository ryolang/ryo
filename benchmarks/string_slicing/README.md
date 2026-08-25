# String Slicing Benchmark

**Focus:** Zero-copy views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through `strview` slices counting `fox` occurrences — every comparison is a view into the original buffer; nothing is copied or stored. This is the projection pattern from M8.4: views flow in, scanning allocates nothing.

**Languages compared:** Ryo only (AOT vs JIT). Cross-language comparators are added per-benchmark when a phase needs that measurement.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-25 (post-ABI). Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux). Pre-ABI rows are the same-day checkpoint recorded before commit `7d0a047`.

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Ryo (AOT)** | 5.4 ms ± 0.1 ms | 2.75 MB |
| **Ryo (JIT)** | 10.1 ms ± 0.5 ms | 6.50 MB |
| Ryo (AOT), pre-ABI | 4.9 ms ± 0.4 ms | 2.75 MB |
| Ryo (JIT), pre-ABI | 6.6 ms ± 0.2 ms | 6.34 MB |

Note: the JIT time moved from ~6.6 ms to ~10 ms with the packed-`u128` ABI while AOT stayed flat; the delta reproduced across runs on the same day. Cause not yet investigated.

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
