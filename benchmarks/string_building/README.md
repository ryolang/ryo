# String Building Benchmark

**Focus:** Runtime string ABI + eager destruction. Concat over 50,000 iterations (`s = s + "x"`): every iteration allocates a fresh buffer through `ryo_str_concat` and eagerly frees the previous one at the reassign. This is the direct before/after measure for the Phase 0 runtime ABI change (packed-`u128` return-by-value) — the ABI decision and its rationale are recorded on `pack_pair` in `runtime/src/lib.rs` and pinned by the `clif_string_ops_use_packed_return_no_stack_slots` integration test.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Why Ryo trails here: value-semantic concat (and the planned fix)

The two programs do different work. Rust's `s.push_str("x")` appends with amortized growth — capacity doubling means ~17 reallocs total. Ryo's `s = s + "x"` is value-semantic: `ryo_str_concat` constructs a **fresh exact-size buffer every iteration**, copies the whole current string into it, and eager destruction frees the old buffer at the reassign. Iteration *i* copies *i* bytes, so the loop copies ~1.25 GB in total — that O(n²) churn is the entire ~12.5× gap, not codegen quality.

This is deliberately **not** filed as a compiler issue: nothing is miscompiled — the copying is the honest cost of asking for a fresh value per iteration. The amortized fast path already exists as `str_push(&s, "x")` (capacity growth via `__ryo_str_push`, `runtime/src/lib.rs:382`); this benchmark intentionally measures the concat + eager-free path (the ABI / eager-destruction measure), not the fastest way to build a string in Ryo.

The planned fix lives in the roadmap's SSO/COW work (see `docs/dev/implementation_roadmap.md` → *Standard Library Allocation Optimizations*, and `docs/dev/stdlib_optimizations.md`): once `str` carries COW refcounts and growth capacity, `s = s + suffix` on a uniquely-referenced buffer becomes an in-place append — realloc-or-extend plus copying the suffix only — turning this loop amortized O(n), at `push_str` parity, without changing the language. The ownership pass already proves the old binding dead at the concat, and a reassignable `s` provably has no live views, so the uniqueness side of the check is compiler-known; what is missing is the allocation policy (today every buffer is exact-size, `cap == len`). This benchmark is the tracking measure for that win: the gap should collapse when the SSO/COW entry lands.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Mean time | vs fastest | Max RSS |
|---|---|---|---|
| **Rust** | 1.4 ms ± 0.1 ms | 1.00x | 1.61 MB |
| **Swift** | 2.3 ms ± 0.4 ms | 1.61x slower | 1.81 MB |
| **Ryo (AOT)** | 17.8 ms ± 0.3 ms | 12.46x slower | 2.25 MB |
| **Ryo (JIT)** | 19.4 ms ± 0.3 ms | 13.52x slower | 5.84 MB |

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
