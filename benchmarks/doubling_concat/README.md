# Doubling Concat Benchmark

**Focus:** Runtime allocation strategy under exponential growth. `s = s + s` for 20 iterations (16 B → 16 MiB): each doubling allocates a fresh `cap == len` buffer through `ryo_str_concat` / `ryo_str_alloc` and eagerly frees the previous one, so live string memory stays bounded by the old-plus-new buffers (≈1.5× the final size at the peak doubling) instead of growing with the history; measured total RSS below is ~2× the final size once baseline process and allocator overhead are included.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 3.7 ms ± 0.2 ms | 1.06x slower | 35.64 MB |
| **Swift** | 6.3.3 | 4.0 ms ± 0.1 ms | 1.13x slower | 34.03 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+b3b7d25 | 3.5 ms ± 0.1 ms | 1.00x | 33.42 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+b3b7d25 | 4.7 ms ± 0.6 ms | 1.34x slower | 37.03 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
