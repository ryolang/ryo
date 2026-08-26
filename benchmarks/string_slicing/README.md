# String Slicing Benchmark

**Focus:** Zero-copy views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through `strview` slices counting `fox` occurrences — every comparison is a view into the original buffer; nothing is copied or stored. This is the projection pattern from M8.4: views flow in, scanning allocates nothing.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26 (post-ABI). Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux). Pre-ABI rows are the same-day checkpoint recorded before commit `7d0a047`.

| Candidate | Mean time | vs fastest | Max RSS |
|---|---|---|---|
| **Rust** | 1.7 ms ± 0.1 ms | 1.00x | 2.95 MB |
| **Swift** | 2.6 ms ± 0.2 ms | 1.53x slower | 7.11 MB |
| **Ryo (AOT)** | 5.3 ms ± 0.2 ms | 3.12x slower | 2.75 MB |
| **Ryo (JIT)** | 10.1 ms ± 0.7 ms | 5.94x slower | 6.47 MB |
| Ryo (AOT), pre-ABI | 4.9 ms ± 0.4 ms | 2.88x slower | 2.75 MB |
| Ryo (JIT), pre-ABI | 6.6 ms ± 0.2 ms | 3.88x slower | 6.34 MB |

Note: the JIT time moved from ~6.6 ms to ~10 ms with the packed-`u128` ABI while AOT stayed flat; the delta reproduced across runs on the same day. Cause not yet investigated.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
