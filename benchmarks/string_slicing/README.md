# String Slicing Benchmark

**Focus:** Zero-copy views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through `strview` slices counting `fox` occurrences — `count_fox` borrows the `str` and every comparison is a view into the original buffer; nothing is copied or stored.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | vs fastest | Max RSS |
|---|---|---|---|
| **Rust** | 1.7 ms ± 0.3 ms | 1.00x | 2.95 MB |
| **Swift** | 2.5 ms ± 0.1 ms | 1.51x slower | 7.09 MB |
| **Ryo (AOT)** | 5.8 ms ± 0.2 ms | 3.49x slower | 2.75 MB |
| **Ryo (JIT)** | 10.8 ms ± 0.6 ms | 6.57x slower | 6.70 MB |

Note: the JIT time moved from ~6.6 ms to ~10 ms with the packed-`u128` ABI while AOT stayed flat; the delta reproduced across runs on the same day. Cause not yet investigated. Still ~10 ms with the JIT at `opt_level=speed` (2026-08-26), so the gap is not an unoptimized-JIT-codegen artifact.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
