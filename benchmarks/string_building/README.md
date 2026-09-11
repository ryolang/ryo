# String Building Benchmark

**Focus:** Runtime string ABI + eager destruction. Concat over 50,000 iterations (`s = s + "x"`): every iteration allocates a fresh buffer through `ryo_str_concat` and eagerly frees the previous one at the reassign. This is the direct before/after measure for the packed-`u128` string runtime ABI (commit `7d0a047`, return-by-value replacing the per-call-site out-pointer stack slot) — the ABI decision and its rationale are recorded on `pack_pair` in `runtime/src/lib.rs` and pinned by the `clif_string_ops_use_packed_return_no_stack_slots` integration test.

**Languages compared:** Rust, Swift, Ryo (AOT vs JIT), and Python.

## Why Ryo trails here: value-semantic concat (and the planned fix)

The two programs do different work. Rust's `s.push_str("x")` appends with amortized growth — capacity doubling means ~17 reallocs total. Ryo's `s = s + "x"` is value-semantic: `ryo_str_concat` constructs a **fresh exact-size buffer every iteration**, copies the whole current string into it, and eager destruction frees the old buffer at the reassign. Iteration *i* copies *i* bytes, so the loop copies ~1.25 GB in total — that O(n²) churn is the entire ~11.8× gap, not codegen quality.

This is deliberately **not** filed as a compiler issue: nothing is miscompiled — the copying is the honest cost of asking for a fresh value per iteration. The amortized fast path already exists as `str_push(&s, "x")` (capacity growth via `__ryo_str_push`, `runtime/src/lib.rs:382`); this benchmark intentionally measures the concat + eager-free path (the ABI / eager-destruction measure), not the fastest way to build a string in Ryo.

The planned fix lives in the roadmap's SSO/COW work (see `docs/dev/implementation_roadmap.md` → *Standard Library Allocation Optimizations*, and `docs/dev/stdlib_optimizations.md`): once `str` carries COW refcounts and growth capacity, `s = s + suffix` on a uniquely-referenced buffer becomes an in-place append — realloc-or-extend plus copying the suffix only — turning this loop amortized O(n), at `push_str` parity, without changing the language. The ownership pass already proves the old binding dead at the concat, and a reassignable `s` provably has no live views, so the uniqueness side of the check is compiler-known; what is missing is the allocation policy (today every buffer is exact-size, `cap == len`). This benchmark is the tracking measure for that win: the gap should collapse when the SSO/COW entry lands.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 1.4 ms ± 0.0 ms | 1.00x | 1.59 MB |
| **Swift** | 6.3.3 | 2.3 ms ± 0.2 ms | 1.60x slower | 1.81 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+b3b7d25 | 17.1 ms ± 0.2 ms | 11.83x slower | 2.27 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+b3b7d25 | 18.6 ms ± 0.7 ms | 12.91x slower | 5.77 MB |
| **Python** | 3.14.7 | 35.3 ms ± 0.9 ms | 24.47x slower | 14.69 MB |

Python (CPython 3.14.7) runs the same `s += "x"` loop interpreted; its ~25x gap over Rust is interpreter overhead, and its ~2x gap over Ryo shows the interpreted baseline is slower than Ryo's compiled O(n²) concat even before any allocation-policy fix lands.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
