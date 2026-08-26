# String Slicing Benchmark

**Focus:** Zero-copy views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through `strview` slices counting `fox` occurrences — `count_fox` borrows the `str` and every comparison is a view into the original buffer; nothing is copied or stored.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Why Ryo trails here: a runtime call per operation (and the planned fix)

Unlike string_building, this gap is **not** semantic — it is codegen quality, and it is filed as tracked work. Each of the ~700k scan iterations makes four calls across the runtime-library boundary where Rust inlines everything (CLIF-verified 2026-08-26):

1. `__ryo_slice(ptr, len, i, i+3)` — two bounds checks, two UTF-8 char-boundary tests, and a `ptr.add`. Rust's `&text[i..i+3]` is inlined pointer arithmetic.
2. `ryo_str_from_literal("fox", 3)` — re-packs the same `(ptr, len)` every iteration; loop-invariant, but as an opaque extern call no Cranelift pass can hoist it.
3. `ryo_str_eq(...)` — an extern call to compare 3 bytes; LLVM turns Rust's into a load-and-cmp.
4. `ryo_str_free(lit, 0)` — a guaranteed no-op on the literal's cap=0 static sentinel, still emitted per iteration.

Plus three checked-arithmetic guard-and-branch pairs per iteration (`i + 3`, `i + 1`, `count += 1`) — spec §18 mandates them; Rust release wraps silently (same story as fibonacci).

One fairness note: Rust scans raw bytes (`&[u8]`), while Ryo's slice validates UTF-8 char boundaries per spec §3.1 — a mandated check Rust never pays. Inlined, it is two bit tests; across an extern call it is part of the per-iteration call cost above.

**The fix path** (tracked in `ISSUES.md`, no language change): emit the tiny runtime bodies as inline Cranelift IR at the call sites; once `pack_pair` is inlined, literal materialization becomes hoistable loop-invariant code; skip free emission for statically-known cap=0 values; elide overflow guards a value-range analysis proves safe. These should remove most of the call overhead; whatever margin remains after that is Cranelift-vs-LLVM mid-end quality plus the spec-mandated boundary checks. This benchmark is the tracking measure.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | vs fastest | Max RSS |
|---|---|---|---|
| **Rust** | 1.7 ms ± 0.3 ms | 1.00x | 2.95 MB |
| **Swift** | 2.5 ms ± 0.1 ms | 1.51x slower | 7.09 MB |
| **Ryo (AOT)** | 5.8 ms ± 0.2 ms | 3.49x slower | 2.75 MB |
| **Ryo (JIT)** | 10.8 ms ± 0.6 ms | 6.57x slower | 6.70 MB |

Note: the JIT time moved from ~6.6 ms to ~10 ms with the packed-`u128` ABI while AOT stayed flat; the delta reproduced across runs on the same day. Cause not yet investigated. Still ~10 ms with the JIT at `opt_level=speed` (2026-08-26), so the gap is not an unoptimized-JIT-codegen artifact.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
