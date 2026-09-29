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
| **Rust** | 1.98.0 | ~272.2 ms | 1.00x         | 1.45 MB               |
| **Kotlin**| 2.4.20 (java 26.0.2.1) | ~278.8 ms | 1.02x slower | 44.86 MB     |
| **Go**   | 1.27.1 | ~300.5 ms | 1.10x slower  | 3.75 MB               |
| **Swift**| 6.3.3 | ~339.4 ms | 1.25x slower  | 1.56 MB               |
| **Ryo (AOT)** | 0.1.0-dev.20260929+228adbb | ~362.5 ms | 1.33x slower | **1.34 MB**           |
| **Ryo (JIT)** | 0.1.0-dev.20260929+228adbb | ~364.6 ms | 1.34x slower | 4.69 MB               |
| **Bun (TS)**  | 1.3.13 | ~407.4 ms | 1.50x slower | 27.44 MB              |
| **Julia** | 1.12.7 | ~421.4 ms | 1.55x slower | 213.66 MB             |
| **Elixir**    | 1.20.4 | ~975.1 ms | 3.58x slower | 90.20 MB              |
| **Python**| 3.14.4 | ~5.258 s | 19.32x slower  | 19.27 MB               |
| **Ruby** | 4.0.6 | ~5.856 s | 21.51x slower | 18.19 MB              |

*(Measured with `hyperfine` on macOS, Apple M3 Pro, 2026-09-29 (Cranelift 0.136.1 checkpoint). Ryo is compiled using `--release`.)*

### Checkpoint: Cranelift 0.136.1 (2026-09-29)

Re-measurement after the Cranelift pin moved 0.135.1 → 0.136.1 (branch `chore/cranelift-0.136.1`). A disassembly diff of the `fibonacci` hot path across the bump is **byte-identical** — the 0.136.1 mid-end/lowering changes do not touch this shape (the surviving guard is a `sadd_overflow`, and the new upstream flag-forwarding covers only `uadd`/`umul`/`smul`; see I-165). Consistently, Ryo's absolute time is unchanged (~362.5 ms vs ~360.8 ms in the 2026-08-26 checkpoint). The ratio improvement to 1.33× is entirely Rust measuring slower this run (~272 ms vs ~253 ms) along with a machine-wide drift (Swift, Go, and Kotlin all moved the same direction); Ryo AOT max resident (1.34 MB) still holds below Rust's (1.45 MB).

### Checkpoint: value-range guard elision (2026-08-26)

This table is the re-measurement after the value-range guard-elision work landed (see commit `d6aee06`): codegen now seeds a per-function value-range fact map from dominating `if`/`while` comparisons against constants and skips `sadd`/`ssub`/`smul_overflow` guards whose operand bounds make overflow impossible. In the fib hot path, `if n <= 1: return n` proves `n >= 2` on the fall-through, so both `n - 1` and `n - 2` guards are now elided — the per-call hot path drops from 29 to 19 machine instructions on aarch64, matching the unguarded shape; only the outer `fibonacci(n - 1) + fibonacci(n - 2)` addition keeps its overflow guard.

Outcome vs the provisional target (fib(40) AOT ≤ 1.25× Rust): **missed** — Ryo AOT measured ~1.42× Rust (~360.8 ms vs ~253.4 ms; a targeted re-run of just Rust vs Ryo confirmed 1.40×). Ryo's absolute time is roughly unchanged versus the previous same-day baseline (~354.9 ms), so on this host the elided guards — perfectly-predicted not-taken branches — were nearly free; the ratio moved mostly because Rust measured faster. The memory headline holds: Ryo AOT max resident (1.34 MB) stays below Rust's (1.45 MB), the lightest of all languages tested. The remaining checked-arithmetic gap is the surviving outer-add guard, whose unfused `cset` + `tst` + `b.ne` lowering is tracked in `ISSUES.md` (I-165).

See [`../README.md`](../README.md#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.34 MB vs Rust's 1.45 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~4.8 MB).


