# Struct Records Inout Benchmark

**Focus:** Imperative update-in-place through a mutable borrow — the third record-update idiom, alongside the consuming update ([`struct_records`](../struct_records/)) and the keep-original update ([`struct_records_reuse`](../struct_records_reuse/)). Runs 500,000 rounds of `make_person(i)` → `birthday(&p)` → `score(p)` on a `Person{name: str, age: int}` record, where `birthday` mutates the caller's record in place and returns nothing. Each language spells it its own way: Ryo `fn birthday(inout p: Person)` called as `birthday(&p)`, Rust `&mut Person`, Swift `inout Person` (also `&p` at the call site), Go `*Person`, and Python plain attribute mutation on the shared object reference. No new record is created, so no clone, move, retain, or sret return is involved — this isolates the mutable-borrow call itself. **All five languages do the same work and assert the same checksum (27,638,890).**

**Languages compared:** Rust, Swift, Go, Python, and Ryo (AOT vs JIT).

**Why this suite exists:** the inout idiom is Ryo's zero-ceremony answer to "change my record" — mutation is visible at the call site (`&p`), the binding must be `mut`, and Rule 7 rejects aliasing hazards at compile time. This benchmark verifies the performance half of that story: choosing between inout and the consuming move+return form should cost nothing, so users can pick by intent rather than by speed.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Swift** | 6.3.3 | 11.7 ms ± 0.3 ms | 1.00x | 1.56 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+af2cae3 | 20.2 ms ± 0.5 ms | 1.72x slower | 1.34 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+af2cae3 | 22.7 ms ± 0.7 ms | 1.94x slower | 5.22 MB |
| **Rust** | 1.98.0 | 24.4 ms ± 1.7 ms | 2.08x slower | 1.53 MB |
| **Go** | 1.27.1 | 25.1 ms ± 0.6 ms | 2.15x slower | 9.92 MB |
| **Python** | 3.14.7 | 110.3 ms ± 1.5 ms | 9.42x slower | 14.52 MB |

Read against the sibling suites: Ryo AOT matches its own consuming-update time (20.2 ms here vs 20.6 ms in `struct_records` — a wash, as designed) and again beats Rust and Go. The inout form also beats Ryo's keep-original time (28.7 ms in `struct_records_reuse`) by exactly the clone it avoids — the three suites together price Ryo's record-update vocabulary: in-place mutation ≈ consuming update < duplicate-and-modify. Python improves relative to the reuse suite (9.4x vs 12.4x) because in-place mutation skips its object allocation, though it remains an order of magnitude behind. Swift's lead is unchanged and remains the small-string optimization story (I-171). Ryo AOT runs **5.5x faster than Python** with ~11x less memory, at the lightest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `go`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
