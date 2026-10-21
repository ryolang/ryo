# Mandelbrot Benchmark

**Focus:** Float codegen. Mandelbrot escape-iteration sum over a 401×501 grid (max 80 iterations per pixel, `zr`/`zi` scalarized — no complex numbers or arrays in the language yet). No overflow guards are in play for floats, so this is the cleanest readout of raw Cranelift floating-point codegen.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

<!-- BENCH:START -->
<!-- BENCH:END -->

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`. The runner builds the release compiler automatically when it uses the repo default `target/release/ryo` (override with `--ryo-bin PATH` or `$RYO_BIN`).

From the repository root:

```bash
uv run benchmarks/run.py run --benchmarks mandelbrot
```

Add `--dry-run` to build every arm and run only the checksum correctness gate (no hyperfine), or `--languages rust,swift,...` to limit the arms.
