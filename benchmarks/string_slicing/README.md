# String Slicing Benchmark

**Focus:** Zero-copy views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through `strview` slices counting `fox` occurrences — `count_fox` borrows the `str` and every comparison is a view into the original buffer; nothing is copied or stored.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Why Ryo trails here: a runtime call per operation (and the planned fix)

Unlike string_building, this gap is **not** semantic — it is codegen quality, and it is filed as tracked work. Each of the ~700k scan iterations makes two calls across the runtime-library boundary where Rust inlines everything (CLIF-verified 2026-08-26):

1. `__ryo_slice(ptr, len, i, i+3)` — two bounds checks, two UTF-8 char-boundary tests, and a `ptr.add`. Rust's `&text[i..i+3]` is inlined pointer arithmetic.
2. `ryo_str_eq(...)` — an extern call to compare 3 bytes; LLVM turns Rust's into a load-and-cmp.

Two more per-iteration calls used to be on this list and are now removed (2026-08-26, verified by the `clif_str_literal_materialized_once_per_function` and `clif_static_cap_str_free_is_elided` tests): `ryo_str_from_literal("fox", 3)` re-packed the same `(ptr, len)` every iteration — each distinct literal is now materialized once per function in the entry block — and `ryo_str_free(lit, 0)`, a guaranteed no-op on the literal's cap=0 static sentinel, is no longer emitted when the cap is statically 0.

Plus three checked-arithmetic guard-and-branch pairs per iteration (`i + 3`, `i + 1`, `count += 1`) — spec §18 mandates them; Rust release wraps silently (same story as fibonacci).

One fairness note: Rust scans raw bytes (`&[u8]`), while Ryo's slice validates UTF-8 char boundaries per spec §3.1 — a mandated check Rust never pays. Inlined, it is two bit tests; across an extern call it is part of the per-iteration call cost above.

A second, smaller asymmetry: hyperfine times whole processes, so every arm's in-program string build (14 doublings) is included by design — and the Swift arm additionally pays a one-time `[UInt8](s.utf8)` materialization (~0.05 ms measured, ~2% of its total, within run noise) because `String.UTF8View` has no O(1) integer subscript and scanning it directly would be far slower.

**The fix path** (tracked in `ISSUES.md`, no language change): emit the tiny runtime bodies as inline Cranelift IR at the call sites, and elide overflow guards a value-range analysis proves safe. These should remove most of the remaining call overhead; whatever margin remains after that is Cranelift-vs-LLVM mid-end quality plus the spec-mandated boundary checks. This benchmark is the tracking measure.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11 — all rows re-measured after the Rust and Swift arms were rewritten to be idiomatic per the repository convention: Rust now builds the string with `s.repeat(2)` and scans with `text.windows(3).filter(|w| *w == b"fox")`, and Swift hoists the needle array out of the scan loop (previously it transliterated Ryo's `s = s + s` as `s.clone() + &s` and rebuilt a magic `[102, 111, 120]` literal per comparison). Same checksums and semantics; only expression quality changed. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 1.7 ms ± 0.2 ms | 1.00x | 2.88 MB |
| **Swift** | 6.3.3 | 2.6 ms ± 0.2 ms | 1.54x slower | 7.09 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+490b10d | 4.9 ms ± 0.3 ms | 2.94x slower | 2.75 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+490b10d | 6.5 ms ± 0.4 ms | 3.90x slower | 6.62 MB |

### Checkpoint: SSO + consuming concat (2026-09-14)

The string-runtime rework moved this benchmark twice, in opposite directions. (1) Promote-on-view landed with an *unconditional* runtime call: every slice of an owner-typed `str` paid a spill + extern `__ryo_str_ensure_heap` + reload to guarantee the base never moves — 4.9 → 5.7 ms across the ~700k-iteration scan loop. (2) A codegen tag-branch then recovered it: view creation now tests the base's inline tag (top byte of the cap word) and only inline bases take the promote call, while heap and static bases pass their pointer/length straight through — 5.7 → 5.1 ms. The ~0.2 ms residual over the pre-rework 4.9 ms is the per-slice tag test itself; closing it folds into the planned tiny-runtime-op inlining work named above (the same mechanism that will inline `__ryo_slice` and `ryo_str_eq`).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 1.7 ms ± 0.1 ms | 1.00x | 2.88 MB |
| **Swift** | 6.3.3 | 2.7 ms ± 0.1 ms | 1.60x slower | 7.09 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260914+4cef5f9 | 5.1 ms ± 0.2 ms | 3.03x slower | 2.75 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260914+4cef5f9 | 7.4 ms ± 0.7 ms | 4.41x slower | 6.89 MB |

Measurement note: the Ryo rows are quiet-window means at the tagged commit (three runs, 5.1 ms ± 0.2/0.3; full-suite batches under machine load read 5.8–6.0 ms with every arm inflated proportionally). The Rust and Swift rows are from the same-day full-suite run and match their 2026-09-11 values.

### Known tradeoff: growth headroom on doubling concat (2026-09-15)

CodSpeed's memory mode flags this benchmark as a **+48.8% peak-allocation regression** (1.0 → 1.5 MB) after the string-runtime rework — with the allocation count unchanged at 14. The arithmetic is exact: the 14 doubling concats (`s = s + s` on a 43-byte seed) now route through the growth path, and `growth_cap` rounds every buffer up to the next power of two, so iteration *i* allocates 64×2^i bytes instead of exactly 43×2^i — and 64/43 = 1.488. This is the cost side of the same policy that makes `s = s + suffix` amortized O(1) in string_building; a doubling concat is the one append pattern where headroom can **never** be reused (the next iteration always needs 2×len, beyond any constant-factor slack), so the slack is pure overhead here. It does not show up in process RSS: 0.5 MB of heap slack sits under the ~2.7 MB process baseline, and Ryo AOT still measures the lowest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
