# Struct Records Reuse Benchmark

**Focus:** The keep-original record update — what `struct_records` cannot measure. Runs 500,000 rounds of `make_person(i)` → `birthday(p)` → `score(p) + score(q)` on a `Person{name: str, age: int}` record, where the caller **uses `p` again after the update**, so `birthday` cannot consume it. This is the duplicate-and-modify case, and each language pays its own price for keeping both records alive: Rust clones the `String` explicitly (`p.name.clone()` — alloc + memcpy), Swift's value copy retains (SSO keeps the name inline, nearly free), Go shallow-copies the header and shares the immutable backing bytes (GC owns lifetime), Python bumps a refcount, and Ryo writes the clone by hand as `p.name + ""` (borrow the field, concat with empty string, fresh owned `str` — the only way to duplicate a `str` today, since E0043 rejects field moves, structs cannot hold borrows, and there is no `clone` builtin or `shared[T]` yet). **All five languages do the same work and assert the same checksum (54,777,780)** — the difference is purely what "keep the original" costs.

**Languages compared:** Rust, Swift, Go, Python, and Ryo (AOT vs JIT).

**Why this suite exists:** it is the tracking measure for the record-update ergonomics gap (`ISSUES.md` I-172). In the consuming case (`struct_records`) Ryo matches Rust's move semantics and beats it on walltime; here Ryo must clone like Rust, but through an ugly manual idiom, while the share-friendly languages get the update for free. When the `Clone` trait or `shared[T]` lands, this benchmark is where the win shows up.

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11, at Ryo revision `e2db4c6`; later branch commits through `b3b7d25` touch only benchmark files and docs — no compiler code — so these numbers remain directly comparable with suites re-measured at `b3b7d25`. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Swift** | 6.3.3 | 12.0 ms ± 0.7 ms | 1.00x | 1.58 MB |
| **Go** | 1.27.1 | 24.9 ms ± 0.5 ms | 2.08x slower | 9.80 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+e2db4c6 | 28.7 ms ± 0.7 ms | 2.40x slower | 1.36 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+e2db4c6 | 31.3 ms ± 0.7 ms | 2.61x slower | 5.33 MB |
| **Rust** | 1.98.0 | 31.6 ms ± 0.8 ms | 2.64x slower | 1.52 MB |
| **Python** | 3.14.7 | 148.9 ms ± 2.2 ms | 12.44x slower | 14.61 MB |

The ranking is the cost of sharing, read directly. Swift wins because SSO keeps every name inline — its "clone" never touches the heap. Go is second: no clone at all, just a shared header — paid for in GC headroom (9.80 MB RSS, 7x Ryo's). Ryo and Rust both pay a real per-round alloc + memcpy and land together, Ryo AOT ahead of Rust (28.7 vs 31.6 ms) on the strength of cheaper string formatting; Ryo's deficit here is ergonomic, not runtime — `p.name + ""` *is* the clone, and it performs like one. The gap to Swift/Go is what `shared[T]` (retain instead of copy) or a small-string optimization (I-171) would close. Ryo AOT runs **5.2x faster than Python** with ~11x less memory, again at the lightest RSS of the suite.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `go`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
