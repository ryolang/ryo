# String Building Benchmark

**Focus:** Runtime string ABI + eager destruction. Concat over 50,000 iterations (`s = s + "x"`): every iteration allocates a fresh buffer through `ryo_str_concat` and eagerly frees the previous one at the reassign. This is the direct before/after measure for the Phase 0 runtime ABI change (packed-`u128` return-by-value) — the ABI decision and its rationale are recorded on `pack_pair` in `runtime/src/lib.rs` and pinned by the `clif_string_ops_use_packed_return_no_stack_slots` integration test.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Rust** | 1.5 ms ± 0.1 ms | 1.61 MB |
| **Swift** | 2.4 ms ± 0.1 ms | 1.81 MB |
| **Ryo (AOT)** | 18.1 ms ± 0.8 ms | 2.27 MB |
| **Ryo (JIT)** | 19.7 ms ± 0.8 ms | 5.52 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
