# Mandelbrot Benchmark

**Focus:** Float codegen. Mandelbrot escape-iteration sum over a 401×501 grid (max 80 iterations per pixel, `zr`/`zi` scalarized — no complex numbers or arrays in the language yet). No overflow guards are in play for floats, so this is the cleanest readout of raw Cranelift floating-point codegen.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | vs fastest | Max RSS |
|---|---|---|---|
| **Rust** | 13.4 ms ± 0.3 ms | 1.00x | 1.44 MB |
| **Swift** | 14.3 ms ± 1.1 ms | 1.07x slower | 5.55 MB |
| **Ryo (AOT)** | 15.2 ms ± 0.6 ms | 1.13x slower | 1.36 MB |
| **Ryo (JIT)** | 16.1 ms ± 0.6 ms | 1.20x slower | 4.92 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
