# String Building Benchmark

**Focus:** Runtime string ABI + eager destruction. Concat over 50,000 iterations (`s = s + "x"`): every iteration allocates a fresh buffer through `ryo_str_concat` and eagerly frees the previous one at the reassign. This is the direct before/after measure for the Phase 0 runtime ABI change (packed-`u128` return-by-value) — see `docs/superpowers/specs/2026-08-25-phase0-runtime-abi-and-benchmarks-design.md`.

**Languages compared:** Ryo only (AOT vs JIT). Cross-language comparators are added per-benchmark when a phase needs that measurement.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-25. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Ryo (AOT)** | 17.4 ms ± 0.3 ms | 2.28 MB |
| **Ryo (JIT)** | 19.2 ms ± 0.8 ms | 5.53 MB |

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
