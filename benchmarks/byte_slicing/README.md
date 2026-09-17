# Byte Slicing Benchmark

**Focus:** Byte-level zero-copy views. Same workload as [`string_slicing`](../string_slicing/) — build a 688 KiB buffer in-program (doubling concat of a 43-byte seed), then scan it through 3-byte view slices counting `fox` occurrences — but every arm operates on raw bytes: Ryo `bytes`/`bytesview`, Rust `&[u8]`, Swift `[UInt8]`. No UTF-8 char-boundary validation anywhere; this is the like-for-like comparison for byte scanning, split out of `string_slicing` on 2026-09-17 when that benchmark's Rust and Swift arms were converted to string semantics.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## What it isolates

Ryo's `str` slicing validates UTF-8 char boundaries at slice creation (spec §3.1) to keep the `strview` "immutable UTF-8 view" invariant; `bytes` slicing is the intended no-check path. The delta between this benchmark and `string_slicing`'s Ryo rows is exactly that validation cost — about 0.6–0.7 ms across the ~700k-iteration scan loop (2.7 vs 3.4 ms AOT at the 2026-09-17 checkpoint). Everything else (bounds checks, §18 overflow guards, the promote-on-view spill tracked as I-184 in `ISSUES.md`) is identical between the two.

The Ryo arm also exercises the short-literal `==` specialization on the bytes family: `text[i:i+3] == b"fox"` inlines to a length check plus three byte compares, with no `ryo_bytes_eq` call.

One fairness note: the Swift arm pays a one-time `[UInt8](s.utf8)` materialization (~0.05 ms measured, ~2% of its total, within run noise) because Swift has no raw-byte string view with an O(1) integer subscript.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-17. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l`.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 1.6 ms ± 0.1 ms | 1.00x | 2.88 MB |
| **Swift** | 6.3.3 | 2.5 ms ± 0.1 ms | 1.57x slower | 7.09 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260917+63078ac | 2.7 ms ± 0.1 ms | 1.70x slower | 2.75 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260917+63078ac | 4.2 ms ± 0.1 ms | 2.62x slower | 6.97 MB |

Ryo AOT lands within noise of Swift here — the remaining 1.70× to Rust is the §18 checked-arithmetic guards (three per iteration), the promote-on-view per-iteration spill (I-184), and Cranelift-vs-LLVM mid-end quality.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
