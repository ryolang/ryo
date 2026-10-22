# Byte Slicing Benchmark

## Overview

* **Focus:** The same scan workload on raw bytes — `bytesview` / `&[u8]` / `[UInt8]` slices over a 688 KiB in-program-built buffer, with no UTF-8 char-boundary validation anywhere. Split out of string_slicing (2026-09-17) so each suite compares like with like; the delta between the two isolates Ryo's `str` boundary-validation cost.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).
* **What to look for:** the UTF-8 char-boundary validation cost — this is the raw-byte twin of string_slicing, so the Ryo delta between the two suites isolates exactly that; the checkpoint below breaks it down.

## What it isolates

Ryo's `str` slicing validates UTF-8 char boundaries at slice creation (spec §3.1) to keep the `strview` "immutable UTF-8 view" invariant; `bytes` slicing is the intended no-check path. The delta between this benchmark and `string_slicing`'s Ryo rows is exactly that validation cost — about 0.6–0.7 ms across the ~700k-iteration scan loop (2.7 vs 3.4 ms AOT at the 2026-09-17 checkpoint). Everything else (bounds checks, §18 overflow guards, the promote-on-view spill tracked as I-184 in `ISSUES.md`) is identical between the two.

The Ryo arm also exercises the short-literal `==` specialization on the bytes family: `text[i:i+3] == b"fox"` inlines to a length check plus three byte compares, with no `ryo_bytes_eq` call.

One fairness note: the Swift arm pays a one-time `[UInt8](s.utf8)` materialization (~0.05 ms measured, ~2% of its total, within run noise) because Swift has no raw-byte string view with an O(1) integer subscript.

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |
|---|---|---|---|---|---|
| Rust | 1.99.1 | 1.787 ms | 1.00x | 2.908 MB | — |
| Swift | 6.4 | 3.372 ms | 1.89x slower | 7.11 MB | — |
| Ryo (AOT) | 0.1.0-dev.20261022+1012fd4 | 3.409 ms | 1.91x slower | ⭐ 2.76 MB | — |
| Ryo (JIT) | 0.1.0-dev.20261022+1012fd4 | 5.143 ms | 2.88x slower | 7.6 MB | — |

Measured on Darwin/arm64/Mac15,6, 2026-10-22 (commit 1012fd4, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

### Checkpoint: baseline (2026-09-17)

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-17. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l`.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 1.6 ms ± 0.1 ms | 1.00x | 2.88 MB |
| **Swift** | 6.3.3 | 2.5 ms ± 0.1 ms | 1.57x slower | 7.09 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260917+63078ac | 2.7 ms ± 0.1 ms | 1.70x slower | 2.75 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260917+63078ac | 4.2 ms ± 0.1 ms | 2.62x slower | 6.97 MB |

Ryo AOT lands within noise of Swift here — the remaining 1.70× to Rust is the §18 checked-arithmetic guards (three per iteration), the promote-on-view per-iteration spill (I-184), and Cranelift-vs-LLVM mid-end quality.
