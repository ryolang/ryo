# Many Small Strings Benchmark

## Overview

* **Focus:** Flat-loop alloc/free churn — 500,000 short strings built and dropped, complementing eager_destruction's recursion angle.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).
* **What to look for:** how per-allocation string overhead scales across runtimes — the suite's point is whether short-lived short strings ever touch the heap; the SSO checkpoint below has the before/after.

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |
|---|---|---|---|---|---|
| Ryo (AOT) | 0.1.0-dev.20261022+1012fd4 | 8.497 ms | 1.00x | ⭐ 1.37 MB | — |
| Ryo (JIT) | 0.1.0-dev.20261022+1012fd4 | 10.134 ms | 1.19x slower | 5.57 MB | — |
| Rust | 1.99.1 | 10.559 ms | 1.24x slower | 1.537 MB | — |
| Swift | 6.4 | 10.598 ms | 1.25x slower | 1.609 MB | — |

Measured on Darwin/arm64/Mac15,6, 2026-10-22 (commit 1012fd4, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

### Checkpoint: SSO + consuming concat (2026-09-14)

Re-measured after the string-runtime rework: `str` is now a tagged 24-byte slot — inline for ≤ 23-byte strings, heap with growth headroom beyond that, static `.rodata` for literals. `int_to_str(i) + "!"` is at most 8 bytes, so the per-iteration string never touches the heap: no allocation, and its free is a no-op on the inline tag.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Ryo (AOT)** | 0.1.0-dev.20260914+75d0f1e | 9.6 ms ± 0.4 ms | 1.00x | 1.34 MB |
| **Rust** | 1.98.0 | 10.5 ms ± 0.3 ms | 1.10x slower | 1.50 MB |
| **Swift** | 6.3.3 | 10.6 ms ± 0.3 ms | 1.10x slower | 1.58 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260914+75d0f1e | 11.4 ms ± 0.6 ms | 1.18x slower | 5.02 MB |

Ryo AOT went from 19.0 ms (1.86x behind Rust) to 9.6 ms — now the **fastest arm**, ahead of both Rust (10.5 ms) and Swift (10.6 ms), at the lightest RSS. A same-day re-run on a busier machine confirmed the ranking (Ryo AOT 10.7 ms vs Rust 11.5 ms, Swift 11.8 ms).
