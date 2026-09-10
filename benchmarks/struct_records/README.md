# Struct Records Benchmark

**Focus:** Aggregate ABI traffic, idiomatic per language. Runs 500,000 rounds of `make_person(i)` → `birthday(p)` → `score(q)` on a `Person{name: str, age: int}` record — the natural "update one field, keep the rest" struct update. Each language expresses it the way a developer actually would: Rust moves the `name` field out (partial move, free), Swift value-copies it (COW-backed `String`, cheap), Python shares the `str` reference (free), and Ryo takes the parameter by `move` and mutates the field in place (`mut q = p; q.age = q.age + 1`). Ryo rejects moving a *single field* out of a struct (E0043 — fields move only with the whole struct) and has no clone builtin, but whole-struct moves are the idiomatic answer here, so **all four languages do the same work and assert the same checksum (27,638,890)**. Rust runs its drop glue, Swift its ARC retain/release traffic on the `String` field, Python its object model (`__slots__` class), and Ryo its eager-destruction scheduling.

**Languages compared:** Rust, Swift, Python, and Ryo (AOT vs JIT).

## Evolution note: this benchmark must mutate to B

This flat-loop record workload is the interim form. Once `list[T]` lands (M22, see `docs/dev/implementation_roadmap.md`), this benchmark **must** evolve into the AoS *particles* workload: a `list[Particle]` of structs with `x/y/z` float fields, integrated over N time steps — the array-of-structs layout comparison Ryo vs Rust vs Swift vs Python. That is the comparison that actually exercises contiguous aggregate storage, iteration over struct elements, and in-place field mutation at scale. When it does, this synthetic loop retires or survives as the flat-loop baseline leg.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Swift** | 6.3.3 | 11.6 ms ± 0.5 ms | 1.00x | 1.56 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+6e17e26 | 20.1 ms ± 0.5 ms | 1.73x slower | 1.39 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+6e17e26 | 22.6 ms ± 0.7 ms | 1.95x slower | 5.17 MB |
| **Rust** | 1.98.0 | 23.9 ms ± 1.1 ms | 2.06x slower | 1.53 MB |
| **Python** | 3.14.7 | 130.7 ms ± 1.7 ms | 11.27x slower | 14.52 MB |

Ryo AOT **beats Rust** here (20.1 ms vs 23.9 ms). The earlier Ryo arm re-derived the name per round because field-level moves are rejected (E0043); rewriting `birthday` to take the record by `move` and mutate the field in place — the idiomatic Ryo shape — removed the per-round `int_to_str` + concat + alloc/free entirely (36.9 ms → 20.1 ms) and unified the checksum across all four languages. What remains is pure aggregate ABI traffic, where Ryo's eager-destruction scheduling and sret returns hold up well; Rust additionally pays `format!` machinery per round. Swift still wins outright: its small-string optimization keeps every name inline (≤ 10 UTF-8 bytes) and its value copy is cheap — closing that is tracked as the small-string optimization work in `ISSUES.md`. Ryo AOT runs **6.5x faster than Python** with ~10x less memory, at the lightest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
