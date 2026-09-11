# String Building Benchmark

**Focus:** Runtime string ABI + eager destruction. Concat over 50,000 iterations (`s = s + "x"` — now spelled identically in Rust and Ryo): every iteration allocates a fresh buffer through `ryo_str_concat` and eagerly frees the previous one at the reassign. This is the direct before/after measure for the packed-`u128` string runtime ABI (commit `7d0a047`, return-by-value replacing the per-call-site out-pointer stack slot) — the ABI decision and its rationale are recorded on `pack_pair` in `runtime/src/lib.rs` and pinned by the `clif_string_ops_use_packed_return_no_stack_slots` integration test.

**Languages compared:** Rust, Swift, Ryo (AOT vs JIT), and Python.

## Why Ryo trails here: same source, different allocation policy

The Rust and Ryo arms are now the *identical* program — both are `s = s + "x"` in a loop — so the ~12x gap is entirely runtime semantics, not algorithm choice. Rust's `impl Add<&str> for String` **consumes the left-hand side and reuses its buffer** (documented std behavior): ownership moves into the operator, uniqueness is proven by the type system, and the append happens in place with amortized capacity growth (~17 reallocs total, O(n)). Ryo's `s = s + "x"` calls `ryo_str_concat`, which constructs a **fresh exact-size buffer every iteration**, copies the whole current string into it, and eager destruction frees the old buffer at the reassign. Iteration *i* copies *i* bytes, so the loop copies ~1.25 GB in total — that O(n²) churn is the entire gap, not codegen quality.

The sharper learning (2026-09-11): Ryo doesn't need COW refcounts to close this. The ownership pass already proves statically what Rust's type system proves — at a reassign concat the old binding is dead, and a reassignable `s` provably has no live views — so in-place append is sound for exactly this pattern. What is missing is purely allocation policy: Ryo buffers are always exact-size (`cap == len`, no growth headroom), and concat never attempts to extend the lhs buffer. This is filed as tracked work in `ISSUES.md` (the consuming-concat in-place-append entry, complementing the small-string entry that redesigns the same slot layout): route a provably-consuming `s = s + suffix` through the `__ryo_str_push`-style growth path — realloc-or-extend, copy the suffix only — turning this loop amortized O(n) with no source change. The SSO/COW roadmap work (`docs/dev/implementation_roadmap.md` → *Standard Library Allocation Optimizations*, `docs/dev/stdlib_optimizations.md`) then generalizes the win beyond the consuming case. This benchmark is the tracking measure: the gap should collapse when the entries land.

The amortized fast path also already exists explicitly as `str_push(&s, "x")` (capacity growth via `__ryo_str_push`, `runtime/src/lib.rs:382`); this benchmark intentionally measures the concat + eager-free path (the ABI / eager-destruction measure), not the fastest way to build a string in Ryo.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 1.4 ms ± 0.0 ms | 1.00x | 1.61 MB |
| **Swift** | 6.3.3 | 2.4 ms ± 0.1 ms | 1.64x slower | 1.81 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+f25e95a | 17.7 ms ± 1.5 ms | 12.33x slower | 2.25 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+f25e95a | 18.6 ms ± 0.4 ms | 12.95x slower | 5.73 MB |
| **Python** | 3.14.7 | 36.1 ms ± 1.4 ms | 25.11x slower | 14.75 MB |

Python (CPython 3.14.7) runs the same `s += "x"` loop interpreted; its ~25x gap over Rust is interpreter overhead, and its ~2x gap over Ryo shows the interpreted baseline is slower than Ryo's compiled O(n²) concat even before any allocation-policy fix lands.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
