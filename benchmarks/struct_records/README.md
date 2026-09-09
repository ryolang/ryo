# Struct Records Benchmark

**Focus:** Aggregate ABI traffic. Runs 500,000 rounds of `make_person(i)` → `birthday(p)` → `score(q)` on a `Person{name: str, age: int}` record. Every round constructs a struct, consumes it into a new struct, and reads it back — stressing struct return conventions (sret), field-wise copies, and drop glue across a heap-allocated `str` field plus a Copy `int` field. Rust runs its drop glue, Swift its ARC retain/release traffic on the `String` field, Python its object model (`__slots__` class), and Ryo its eager-destruction scheduling.

**Languages compared:** Rust, Swift, Python, and Ryo (AOT vs JIT).

## Evolution note: this benchmark must mutate to B

This flat-loop record workload is the interim form. Once `list[T]` lands (M22, see `docs/dev/implementation_roadmap.md`), this benchmark **must** evolve into the AoS *particles* workload: a `list[Particle]` of structs with `x/y/z` float fields, integrated over N time steps — the array-of-structs layout comparison Ryo vs Rust vs Swift vs Python. That is the comparison that actually exercises contiguous aggregate storage, iteration over struct elements, and in-place field mutation at scale. When it does, this synthetic loop retires or survives as the flat-loop baseline leg.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-09. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Swift** | 6.3.3 | 15.2 ms ± 3.6 ms | 1.00x | 1.56 MB |
| **Rust** | 1.98.0 | 37.9 ms ± 1.2 ms | 2.49x slower | 1.52 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260909+9b92b25 | 38.1 ms ± 2.4 ms | 2.50x slower | 1.39 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260909+9b92b25 | 40.5 ms ± 4.1 ms | 2.66x slower | 5.33 MB |
| **Python** | 3.14.7 | 173.5 ms ± 12.4 ms | 11.39x slower | 14.50 MB |

Ryo AOT lands neck-and-neck with Rust (within noise) at the lightest RSS of the suite — the struct ABI traffic (sret returns, field copies, eager drops) costs the same as Rust's move-and-drop-glue path. Both trail Swift ~2.5x on this workload for a reason unrelated to aggregates: every name here is ≤ 10 UTF-8 bytes, so Swift's small-string optimization keeps the `String` inline in the struct while Rust's `String` and Ryo's `str` heap-allocate each one — the same effect visible in `many_small_strings`. Ryo AOT runs **4.6x faster than Python** with ~10x less memory.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
