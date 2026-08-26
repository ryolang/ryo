# String Building Benchmark

**Focus:** Runtime string ABI + eager destruction. Concat over 50,000 iterations (`s = s + "x"`): every iteration allocates a fresh buffer through `ryo_str_concat` and eagerly frees the previous one at the reassign. This is the direct before/after measure for the Phase 0 runtime ABI change (packed-`u128` return-by-value) — the ABI decision and its rationale are recorded on `pack_pair` in `runtime/src/lib.rs` and pinned by the `clif_string_ops_use_packed_return_no_stack_slots` integration test.

**Languages compared:** Ryo only (AOT vs JIT). Cross-language comparators are added per-benchmark when a phase needs that measurement.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-25 (post-ABI). Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux). Pre-ABI rows are the same-day checkpoint recorded before commit `7d0a047`.

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Ryo (AOT)** | 18.1 ms ± 0.5 ms | 2.28 MB |
| **Ryo (JIT)** | 19.6 ms ± 0.9 ms | 5.53 MB |
| Ryo (AOT), pre-ABI | 17.4 ms ± 0.3 ms | 2.28 MB |
| Ryo (JIT), pre-ABI | 19.2 ms ± 0.8 ms | 5.53 MB |

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
