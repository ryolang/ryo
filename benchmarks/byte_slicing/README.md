# Byte Slicing Benchmark

## Overview

* **Focus:** The same scan workload on raw bytes — `bytesview` / `&[u8]` / `[UInt8]` slices with no UTF-8 char-boundary validation anywhere. Split out of string_slicing (2026-09-17) so each suite compares like with like; the delta between the two isolates Ryo's `str` boundary-validation cost.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).
* **Highlights:** Ryo AOT (2.7 ms) lands **within noise of Swift** (2.5 ms) at 1.70x behind Rust; the ~0.6–0.7 ms delta against Ryo's string_slicing row is exactly the UTF-8 char-boundary validation cost.

**Focus:** Byte-level zero-copy views. Same workload as [`string_slicing`](../string_slicing/) — build a 688 KiB buffer in-program (doubling concat of a 43-byte seed), then scan it through 3-byte view slices counting `fox` occurrences — but every arm operates on raw bytes: Ryo `bytes`/`bytesview`, Rust `&[u8]`, Swift `[UInt8]`. No UTF-8 char-boundary validation anywhere; this is the like-for-like comparison for byte scanning, split out of `string_slicing` on 2026-09-17 when that benchmark's Rust and Swift arms were converted to string semantics.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## What it isolates

Ryo's `str` slicing validates UTF-8 char boundaries at slice creation (spec §3.1) to keep the `strview` "immutable UTF-8 view" invariant; `bytes` slicing is the intended no-check path. The delta between this benchmark and `string_slicing`'s Ryo rows is exactly that validation cost — about 0.6–0.7 ms across the ~700k-iteration scan loop (2.7 vs 3.4 ms AOT at the 2026-09-17 checkpoint). Everything else (bounds checks, §18 overflow guards, the promote-on-view spill tracked as I-184 in `ISSUES.md`) is identical between the two.

The Ryo arm also exercises the short-literal `==` specialization on the bytes family: `text[i:i+3] == b"fox"` inlines to a length check plus three byte compares, with no `ryo_bytes_eq` call.

One fairness note: the Swift arm pays a one-time `[UInt8](s.utf8)` materialization (~0.05 ms measured, ~2% of its total, within run noise) because Swift has no raw-byte string view with an O(1) integer subscript.

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
<!-- BENCH:END -->

Ryo AOT lands within noise of Swift here — the remaining 1.70× to Rust is the §18 checked-arithmetic guards (three per iteration), the promote-on-view per-iteration spill (I-184), and Cranelift-vs-LLVM mid-end quality.
