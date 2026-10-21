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
<!-- BENCH:END -->
