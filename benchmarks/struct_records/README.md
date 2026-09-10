# Struct Records Benchmark

**Focus:** Aggregate ABI traffic, idiomatic per language. Runs 500,000 rounds of `make_person(i)` → `birthday(p)` → `score(q)` on a `Person{name: str, age: int}` record — the natural "update one field, keep the rest" struct update. Each language expresses it the way a developer actually would: Rust moves the `name` field out (partial move, free), Swift value-copies it (COW-backed `String`, cheap), Python shares the `str` reference (free). **Ryo cannot**: it rejects moving a single field out of a struct (E0043 — fields move only with the whole struct) and has no clone builtin, so `birthday` re-derives the name (`"user" + int_to_str(p.age)`), paying an extra `int_to_str` + concat + alloc/free per round. That asymmetry is deliberate and is part of what the benchmark measures: the real cost of Ryo's no-partial-move rule, not a benchmark artifact. Because the workloads differ, **checksums are per-language** (Rust/Swift/Python assert 27,638,890; Ryo asserts 25,750,000) — timings compare the same *intent*, not identical instruction streams. Rust runs its drop glue, Swift its ARC retain/release traffic on the `String` field, Python its object model (`__slots__` class), and Ryo its eager-destruction scheduling.

**Languages compared:** Rust, Swift, Python, and Ryo (AOT vs JIT).

## Evolution note: this benchmark must mutate to B

This flat-loop record workload is the interim form. Once `list[T]` lands (M22, see `docs/dev/implementation_roadmap.md`), this benchmark **must** evolve into the AoS *particles* workload: a `list[Particle]` of structs with `x/y/z` float fields, integrated over N time steps — the array-of-structs layout comparison Ryo vs Rust vs Swift vs Python. That is the comparison that actually exercises contiguous aggregate storage, iteration over struct elements, and in-place field mutation at scale. When it does, this synthetic loop retires or survives as the flat-loop baseline leg.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Swift** | 6.3.3 | 11.9 ms ± 0.6 ms | 1.00x | 1.56 MB |
| **Rust** | 1.98.0 | 24.0 ms ± 0.9 ms | 2.01x slower | 1.52 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+c5cbbe2 | 36.9 ms ± 2.0 ms | 3.10x slower | 1.39 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+c5cbbe2 | 39.3 ms ± 1.0 ms | 3.30x slower | 5.33 MB |
| **Python** | 3.14.7 | 131.3 ms ± 1.5 ms | 11.02x slower | 14.52 MB |

Read the Ryo row as two stacked costs: the aggregate ABI traffic (sret returns, field copies, eager drops — comparable to Rust's) plus the language-imposed name rebuild. The rebuild is what puts Ryo 1.54x behind Rust here (36.9 ms vs 24.0 ms): Rust's partial move transfers the `String` for free while Ryo allocates, formats, and frees a fresh `str` every round — a concrete measurement of what field-level moves (or a `clone` builtin) would buy. Swift wins outright because its small-string optimization keeps every name inline (≤ 10 UTF-8 bytes) *and* its value copy is cheap. Ryo AOT still runs **3.6x faster than Python** with ~10x less memory, at the lightest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
