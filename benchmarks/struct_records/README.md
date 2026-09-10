# Struct Records Benchmark

**Focus:** Aggregate ABI traffic, idiomatic per language. Runs 500,000 rounds of `make_person(i)` → `birthday(p)` → `score(q)` on a `Person{name: str, age: int}` record — the natural "update one field, keep the rest" struct update. Each language expresses it the way a developer actually would: Rust moves the `name` field out (partial move, free), Swift value-copies it (COW-backed `String`, cheap), Go shallow-copies the struct (string header shares immutable backing bytes; GC owns lifetime), Python shares the `str` reference (free), and Ryo takes the parameter by `move` and mutates the field in place (`mut q = p; q.age = q.age + 1`). Ryo rejects moving a *single field* out of a struct (E0043 — fields move only with the whole struct) and has no clone builtin, but whole-struct moves are the idiomatic answer here, so **all five languages do the same work and assert the same checksum (27,638,890)**. Rust runs its drop glue, Swift its ARC retain/release traffic on the `String` field, Go its GC, Python its object model (`__slots__` class), and Ryo its eager-destruction scheduling.

**Languages compared:** Rust, Swift, Go, Python, and Ryo (AOT vs JIT).

## Evolution note: this benchmark must mutate to B

This flat-loop record workload is the interim form. Once `list[T]` lands (M22, see `docs/dev/implementation_roadmap.md`), this benchmark **must** evolve into the AoS *particles* workload: a `list[Particle]` of structs with `x/y/z` float fields, integrated over N time steps — the array-of-structs layout comparison Ryo vs Rust vs Swift vs Python. That is the comparison that actually exercises contiguous aggregate storage, iteration over struct elements, and in-place field mutation at scale. When it does, this synthetic loop retires or survives as the flat-loop baseline leg.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Swift** | 6.3.3 | 11.8 ms ± 0.4 ms | 1.00x | 1.56 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+a151528 | 20.6 ms ± 0.6 ms | 1.74x slower | 1.39 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+a151528 | 22.8 ms ± 0.6 ms | 1.94x slower | 5.36 MB |
| **Rust** | 1.98.0 | 24.4 ms ± 0.6 ms | 2.07x slower | 1.52 MB |
| **Go** | 1.27.1 | 25.6 ms ± 0.8 ms | 2.17x slower | 9.30 MB |
| **Python** | 3.14.7 | 132.2 ms ± 2.1 ms | 11.21x slower | 14.59 MB |

Ryo AOT **beats both Rust and Go** here (20.6 ms vs 24.4 / 25.6 ms). The earlier Ryo arm re-derived the name per round because field-level moves are rejected (E0043); rewriting `birthday` to take the record by `move` and mutate the field in place — the idiomatic Ryo shape — removed the per-round `int_to_str` + concat + alloc/free entirely (36.9 ms → ~20 ms) and unified the checksum across all five languages. What remains is pure aggregate ABI traffic, where Ryo's eager-destruction scheduling and sret returns hold up well; Rust additionally pays `format!` machinery per round, and Go pays its GC twice over — in walltime (write barriers, allocation pacing) and most visibly in memory (9.30 MB RSS vs Ryo's 1.39 MB, the classic GC headroom tax). Swift still wins outright: its small-string optimization keeps every name inline (≤ 10 UTF-8 bytes) and its value copy is cheap — closing that is tracked as the small-string optimization work in `ISSUES.md`. Ryo AOT runs **6.4x faster than Python** with ~10x less memory, at the lightest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `go`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
