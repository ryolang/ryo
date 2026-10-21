# String Slicing Benchmark

**Focus:** Zero-copy string views. Builds a 688 KiB string in-program (doubling concat of a 43-byte seed), then scans it through string slices counting `fox` occurrences — `count_fox` borrows the string and every comparison is a view into the original buffer; nothing is copied or stored. Every arm is **string-semantic and idiomatic** (per the repository convention): Ryo `strview` byte-offset slices with UTF-8 char-boundary validation, Rust `&str` direct slicing (same boundary validation, panics on a split character), Swift `Substring` windows walked by `String.Index`. One documented divergence: Swift's window is 3 **Characters** (its `String.Index` cannot split a Character — boundary correctness is structural, not a paid check), while Ryo and Rust slice 3 **bytes** with validation; for this ASCII-only input the windows coincide. For the raw-byte variant of the same workload (`bytesview` / `&[u8]` / `[UInt8]`, no UTF-8 semantics anywhere) see [`byte_slicing`](../byte_slicing/).

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Why Ryo trails Rust here

The original gap was **not** semantic — it was codegen quality. Each of the ~700k scan iterations made two calls across the runtime-library boundary where Rust inlines everything: `__ryo_slice(ptr, len, i, i+3)` (two bounds checks, two UTF-8 char-boundary tests, a `ptr.add`) and `ryo_str_eq(...)` (an extern call to compare 3 bytes). As of 2026-09-17 both bodies — plus literal packing, which was pure `pack_pair` — are emitted as inline Cranelift IR at the call site, and the packed-u128 pair ABI they returned is gone entirely. The scan loop now makes zero runtime calls.

What remains, in rough order of cost:

1. Three checked-arithmetic guard-and-branch pairs per iteration (`i + 3`, `i + 1`, `count += 1`) — spec §18 mandates them; Rust release wraps silently (same story as fibonacci). Value-range guard elision and fused flag branches are tracked work in `ISSUES.md`.
2. The promote-on-view per-iteration spill: every slice of the promoted base re-stores the owner triple and re-branches on the spilled flag (~12 aarch64 instructions per iteration for a loop-invariant base) — tracked as I-184 in `ISSUES.md`.
3. Cranelift-vs-LLVM mid-end quality on what is left.

The spec-mandated UTF-8 char-boundary validation per slice (spec §3.1) is no longer a differentiator: the Rust arm slices `&str` directly (panicking on a split character — the same contract as Ryo), and the Swift arm pays more, not less: its idiomatic `Substring`-by-`String.Index` scan walks grapheme clusters, so boundary correctness is structural but Character iteration costs it dearly. On string semantics Ryo AOT (3.4 ms) sits between Rust (1.8 ms) and Swift (17.6 ms).

One fairness note: hyperfine times whole processes, so every arm's in-program string build (14 doublings) is included by design.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-17 — first run with all arms string-semantic and idiomatic; before this date the Rust and Swift arms scanned raw bytes, so older tables in git history are not comparable (that workload now lives in [`byte_slicing`](../byte_slicing/)). Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
<!-- BENCH:END -->

## Known tradeoff: growth headroom on doubling concat (2026-09-15)

CodSpeed's memory mode flags this benchmark as a **+48.8% peak-allocation regression** (1.0 → 1.5 MB) after the string-runtime rework — with the allocation count unchanged at 14. The arithmetic is exact: the 14 doubling concats (`s = s + s` on a 43-byte seed) now route through the growth path, and `growth_cap` rounds every buffer up to the next power of two, so iteration *i* allocates 64×2^i bytes instead of exactly 43×2^i — and 64/43 = 1.488. This is the cost side of the same policy that makes `s = s + suffix` amortized O(1) in string_building; a doubling concat is the one append pattern where headroom can **never** be reused (the next iteration always needs 2×len, beyond any constant-factor slack), so the slack is pure overhead here. It does not show up in process RSS: 0.5 MB of heap slack sits under the ~2.7 MB process baseline, and Ryo AOT still measures the lowest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`. The runner builds the release compiler automatically when it uses the repo default `target/release/ryo` (override with `--ryo-bin PATH` or `$RYO_BIN`).

From the repository root:

```bash
uv run benchmarks/run.py run --benchmarks string_slicing
```

Add `--dry-run` to build every arm and run only the checksum correctness gate (no hyperfine), or `--languages rust,swift,...` to limit the arms.
