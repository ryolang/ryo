# Fibonacci Benchmarks

These benchmarks compare a standard recursive calculation of `fibonacci(40)` across different languages using `hyperfine`. 

*Note: Ryo's recursive function capability works correctly and is competitive with the natively-compiled languages; see the table below and the note on checked arithmetic in [`../README.md`](../README.md).*

## Prerequisites
Before running the benchmarks, ensure you have the following tools installed and available in your PATH:
- `rustc`
- `go`
- `swiftc`
- `uv` (for Python 3.14)
- `bun`
- `elixir`
- `ruby` (installed with homebrew)
- `julia`
- `kotlinc` and `temurin` (jvm)
- `hyperfine`

You must also have a built Ryo compiler binary. By default, `run_benchmarks.sh` expects the `ryo` binary to be built in release mode at `../../target/release/ryo`. You can ensure this by running `cargo build --release` from the repository root before starting the benchmarks.

## Setup
In the `benchmarks/fibonacci` directory, run:
```bash
./run_benchmarks.sh
```

## Results
Calculating the 40th Fibonacci number recursively (Time taken):

| Language | Version | Mean Time | Speed vs Rust | Memory (Max Resident) |
|----------|---------|-----------|---------------|-----------------------|
| **Rust** | 1.98.0 | ~253.4 ms | 1.00x         | 1.45 MB               |
| **Kotlin**| 2.4.10 (java 26.0.2) | ~270.8 ms | 1.07x slower | 44.55 MB     |
| **Go**   | 1.26.6 | ~292.5 ms | 1.15x slower  | 4.03 MB               |
| **Swift**| 6.3.3 | ~320.5 ms | 1.26x slower  | 1.56 MB               |
| **Ryo (AOT)** | 0.1.0 | ~360.8 ms | 1.42x slower | **1.34 MB**           |
| **Ryo (JIT)** | 0.1.0 | ~361.4 ms | 1.43x slower | 4.75 MB               |
| **Bun (TS)**  | 1.3.13 | ~399.6 ms | 1.58x slower | 27.44 MB              |
| **Julia** | 1.12.6 | ~416.5 ms | 1.64x slower | 214.08 MB             |
| **Elixir**    | 1.20.3 | ~869.3 ms | 3.43x slower | 89.98 MB              |
| **Python**| 3.14.4 | ~4.939 s | 19.49x slower  | 18.97 MB               |
| **Ruby** | 4.0.6 | ~5.863 s | 23.14x slower | 18.19 MB              |

*(Measured with `hyperfine` on macOS, Apple M3 Pro, 2026-08-26 (evening checkpoint). Ryo is compiled using `--release`.)*

### Phase 1 checkpoint: value-range guard elision (2026-08-26)

This table is the post-Phase-1 re-measurement. Phase 1 (value-range guard elision) landed: codegen now seeds a per-function value-range fact map from dominating `if`/`while` comparisons against constants and skips `sadd`/`ssub`/`smul_overflow` guards whose operand bounds make overflow impossible. In the fib hot path, `if n <= 1: return n` proves `n >= 2` on the fall-through, so both `n - 1` and `n - 2` guards are now elided — the per-call hot path drops from 29 to 19 machine instructions on aarch64, matching the unguarded shape; only the outer `fibonacci(n - 1) + fibonacci(n - 2)` addition keeps its overflow guard.

Outcome vs the provisional target (fib(40) AOT ≤ 1.25× Rust): **missed** — Ryo AOT measured ~1.42× Rust (~360.8 ms vs ~253.4 ms; a targeted re-run of just Rust vs Ryo confirmed 1.40×). Ryo's absolute time is roughly unchanged versus the 1.35× re-baseline (~354.9 ms, same machine, earlier the same day), so on this host the elided guards — perfectly-predicted not-taken branches — were nearly free; the ratio moved mostly because Rust measured faster. The memory headline holds: Ryo AOT max resident (1.34 MB) stays below Rust's (1.45 MB), the lightest of all languages tested. Per the roadmap, a missed target triggers re-estimation of the remaining checked-arithmetic gap (the surviving outer-add guard, plus Cranelift's non-fused `cset` + `tst` + `b.ne` overflow check), not failure.

See [`../README.md`](../README.md#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.34 MB vs Rust's 1.45 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~4.8 MB).


