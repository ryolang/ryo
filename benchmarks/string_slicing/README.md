# String Slicing Benchmark

**Focus:** Zero-copy views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through `strview` slices counting `fox` occurrences — `count_fox` borrows the `str` and every comparison is a view into the original buffer; nothing is copied or stored.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Why Ryo trails here: what remains after inlining the tiny runtime ops

The original gap was **not** semantic — it was codegen quality. Each of the ~700k scan iterations made two calls across the runtime-library boundary where Rust inlines everything: `__ryo_slice(ptr, len, i, i+3)` (two bounds checks, two UTF-8 char-boundary tests, a `ptr.add`) and `ryo_str_eq(...)` (an extern call to compare 3 bytes). As of 2026-09-17 both bodies — plus literal packing, which was pure `pack_pair` — are emitted as inline Cranelift IR at the call site, and the packed-u128 pair ABI they returned is gone entirely (see the checkpoint below). The scan loop now makes zero runtime calls.

What remains, in rough order of cost:

1. Three checked-arithmetic guard-and-branch pairs per iteration (`i + 3`, `i + 1`, `count += 1`) — spec §18 mandates them; Rust release wraps silently (same story as fibonacci). Value-range guard elision and fused flag branches are tracked work in `ISSUES.md`.
2. The spec-mandated UTF-8 char-boundary validation per slice (spec §3.1) — two bit tests inlined; Rust scans raw bytes (`&[u8]`) and never pays it.
3. Cranelift-vs-LLVM mid-end quality on what is left.

One fairness note: hyperfine times whole processes, so every arm's in-program string build (14 doublings) is included by design — and the Swift arm additionally pays a one-time `[UInt8](s.utf8)` materialization (~0.05 ms measured, ~2% of its total, within run noise) because `String.UTF8View` has no O(1) integer subscript and scanning it directly would be far slower.

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

### Checkpoint: tiny runtime ops inlined (2026-09-17)

The fix the section above describes landed: `__ryo_slice`/`__ryo_bytes_slice` (bounds + UTF-8 guards, cold `ryo_panic` blocks), literal packing (pure `symbol_value` + `iconst` — `pack_pair` was the whole body), and `==`/`!=` against literals up to 16 bytes (length check + gated per-byte compares) are now inline Cranelift IR at the call site. With no pair-returning runtime call left, the packed-u128 ABI and its ~9-instruction i128 unpack legalization per use are gone, and `enable_llvm_abi_extensions` is retired with it. AOT 5.1 → 3.4 ms; JIT 7.4 → 4.7 ms. The remaining ~2.3× vs Rust is the spec-mandated UTF-8 boundary checks, the §18 overflow guards (elision/fusing tracked in `ISSUES.md`), and Cranelift-vs-LLVM mid-end quality.

| Candidate | Version | Mean time | vs fastest |
|---|---|---|---|
| **Rust** | 1.98.0 | 1.5 ms ± 0.0 ms | 1.00x |
| **Ryo (AOT)** | 0.1.0-dev.20260917+c308a82 | 3.4 ms ± 0.1 ms | 2.27x slower |
| **Ryo (JIT)** | 0.1.0-dev.20260917+c308a82 | 4.7 ms ± 0.1 ms | 3.13x slower |

(Rust was re-measured today alongside the Ryo rows; Swift was not re-run for this checkpoint — see the 2026-09-14 checkpoint for its number.)

Measurement note: the Ryo rows are quiet-window means at the tagged commit (three runs each: AOT 5.1 ms ± 0.2, JIT 7.4 ms ± 0.7; full-suite batches under machine load read 5.8–6.0 ms with every arm inflated proportionally). The Rust and Swift rows are from the same-day full-suite run and match their 2026-09-11 values.

### Known tradeoff: growth headroom on doubling concat (2026-09-15)

CodSpeed's memory mode flags this benchmark as a **+48.8% peak-allocation regression** (1.0 → 1.5 MB) after the string-runtime rework — with the allocation count unchanged at 14. The arithmetic is exact: the 14 doubling concats (`s = s + s` on a 43-byte seed) now route through the growth path, and `growth_cap` rounds every buffer up to the next power of two, so iteration *i* allocates 64×2^i bytes instead of exactly 43×2^i — and 64/43 = 1.488. This is the cost side of the same policy that makes `s = s + suffix` amortized O(1) in string_building; a doubling concat is the one append pattern where headroom can **never** be reused (the next iteration always needs 2×len, beyond any constant-factor slack), so the slack is pure overhead here. It does not show up in process RSS: 0.5 MB of heap slack sits under the ~2.7 MB process baseline, and Ryo AOT still measures the lowest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
