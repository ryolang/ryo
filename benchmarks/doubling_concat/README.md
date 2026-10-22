# Doubling Concat Benchmark

## Overview

* **Focus:** Runtime allocation strategy — `s = s + s` exponential growth to 16 MiB, stressing `ryo_str_alloc` / `ryo_str_concat`; eager destruction frees each superseded buffer, so live string memory stays bounded instead of growing with the history.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).
* **What to look for:** allocation policy under exponential growth — whether live memory stays bounded through the doublings rather than accumulating; the checkpoint below has the post-rework re-measurement.

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |
|---|---|---|---|---|---|
| Rust | 1.99.1 | 4.608 ms | 1.00x | 35.689 MB | — |
| Swift | 6.4 | 5.302 ms | 1.15x slower | 34.078 MB | — |
| Ryo (JIT) | 0.1.0-dev.20261022+1012fd4 | 5.995 ms | 1.30x slower | 37.685 MB | — |
| Ryo (AOT) | 0.1.0-dev.20261022+1012fd4 | 6.379 ms | 1.38x slower | ⭐ 33.417 MB | — |

Measured on Darwin/arm64/Mac15,6, 2026-10-22 (commit 1012fd4, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

### Checkpoint: SSO + consuming concat (2026-09-14)

Re-measured after the string-runtime rework (tagged 24-byte slot: inline ≤ 23 B, heap with growth headroom, static `.rodata`; consuming reassign-concat appends in place via the push path — see `string_building`'s checkpoint). Unchanged by design: `s = s + s` uses the lhs buffer as its own suffix, and the in-place path only fires when the suffix is a *different* owner, so every doubling keeps the fresh-buffer allocating path.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Ryo (AOT)** | 0.1.0-dev.20260915+c598378 | 3.7 ms ± 0.3 ms | 1.00x | 33.42 MB |
| **Rust** | 1.98.0 | 3.7 ms ± 0.2 ms | 1.01x slower | 35.64 MB |
| **Swift** | 6.3.3 | 4.2 ms ± 0.6 ms | 1.14x slower | 34.03 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260915+c598378 | 4.8 ms ± 0.8 ms | 1.31x slower | 37.16 MB |

Measurement note: all four rows come from a single full-suite hyperfine run on 2026-09-15 (same protocol for every arm, hyperfine outlier warnings present on Swift/JIT — treat the 1.01x AOT-vs-Rust margin as a tie). The Ryo rows include the scratch-slot fix that followed this checkpoint, which does not touch the doubling path; timings match the 2026-09-14 checkpoint within noise.
