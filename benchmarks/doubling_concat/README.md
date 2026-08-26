# Doubling Concat Benchmark

**Focus:** Runtime allocation strategy under exponential growth. `s = s + s` for 20 iterations (16 B → 16 MiB): each doubling allocates a fresh `cap == len` buffer through `ryo_str_concat` / `ryo_str_alloc` and eagerly frees the previous one, so live string memory stays bounded by the old-plus-new buffers (≈1.5× the final size at the peak doubling) instead of growing with the history; measured total RSS below is ~2× the final size once baseline process and allocator overhead are included.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | Max RSS |
|---|---|---|
| **Rust** | 4.0 ms ± 0.2 ms | 35.64 MB |
| **Swift** | 4.1 ms ± 0.3 ms | 34.05 MB |
| **Ryo (AOT)** | 3.6 ms ± 0.2 ms | 33.42 MB |
| **Ryo (JIT)** | 4.6 ms ± 0.3 ms | 36.75 MB |

## How to Run

Prerequisites: `hyperfine`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
