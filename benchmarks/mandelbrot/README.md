# Mandelbrot Benchmark

## Overview

* **Focus:** Float codegen — 401×501 grid, max 80 iterations per pixel; no overflow guards in play, the cleanest Cranelift readout.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).
* **Highlights:** Ryo AOT is **1.11x slower than Rust** (14.6 vs 13.2 ms) — with no overflow guards in play for floats, this is the cleanest readout of Cranelift floating-point codegen and it is near parity.

**Focus:** Float codegen. Mandelbrot escape-iteration sum over a 401×501 grid (max 80 iterations per pixel, `zr`/`zi` scalarized — no complex numbers or arrays in the language yet). No overflow guards are in play for floats, so this is the cleanest readout of raw Cranelift floating-point codegen.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |
|---|---|---|---|---|---|
| Rust | 1.99.1 | 13.56 ms | 1.00x | 1.493 MB | — |
| Swift | 6.4 | 14.587 ms | 1.08x slower | 5.577 MB | — |
| Ryo (AOT) | 0.1.0-dev.20261022+1012fd4 | 15.45 ms | 1.14x slower | ⭐ 1.37 MB | — |
| Ryo (JIT) | 0.1.0-dev.20261022+1012fd4 | 17.133 ms | 1.26x slower | 5.833 MB | — |

Measured on Darwin/arm64/Mac15,6, 2026-10-22 (commit 1012fd4, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

### Checkpoint: baseline (2026-09-11)

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 13.2 ms ± 0.1 ms | 1.00x | 1.44 MB |
| **Swift** | 6.3.3 | 14.0 ms ± 0.3 ms | 1.06x slower | 5.55 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+b3b7d25 | 14.6 ms ± 0.2 ms | 1.11x slower | 1.36 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+b3b7d25 | 15.8 ms ± 0.2 ms | 1.20x slower | 5.22 MB |
