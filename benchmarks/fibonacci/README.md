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

You must also have a built Ryo compiler binary. The central runner uses the release build at `target/release/ryo` by default and runs `cargo build --release` for you when it does; override the binary with `--ryo-bin PATH` or the `RYO_BIN` environment variable (see the [root benchmark README](../README.md#general-prerequisites) for the full contract).

## Setup
From the repository root, run:
```bash
uv run benchmarks/run.py run --benchmarks fibonacci
```
Add `--dry-run` to build every arm and run only the checksum correctness gate (no hyperfine), or `--languages rust,go,...` to limit the arms.

## Results
Calculating the 40th Fibonacci number recursively (Time taken):

<!-- BENCH:START -->
| Arm | Time | Peak RSS | Version |
|---|---|---|---|
| Rust | 267.655 ms | 1.476 MB | rustc 1.98.0 |
| Kotlin | 280.338 ms | 44.647 MB | kotlinc 2.4.21 |
| Go | 301.492 ms | 3.942 MB | go 1.27.2 |
| Swift | 329.754 ms | 1.609 MB | swiftc 6.4 |
| Ryo (AOT) | 359.464 ms | 1.37 MB | ryo 0.1.0 |
| Ryo (JIT) | 389.487 ms | 5.475 MB | ryo 0.1.0 |
| Bun (TS) | 406.479 ms | 27.433 MB | bun 1.3.13 |
| Elixir | 990.888 ms | 89.569 MB | elixir 1.20.4 |
| Ruby | 6313.35 ms | 28 MB | ruby 2.6.10p210 |
| Python | 5068.66 ms | 16 MB | uv 3.14.4 |
| Julia | 429.82 ms | 193.684 MB | julia 1.13.1 |
Measured on Darwin/arm64/Mac15,6, 2026-10-21 (commit bddd4e1, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

### Checkpoint: Cranelift 0.136.1 (2026-09-29)

Re-measurement after the Cranelift pin moved 0.135.1 → 0.136.1 (branch `chore/cranelift-0.136.1`). A disassembly diff of the `fibonacci` hot path across the bump is **byte-identical** — the 0.136.1 mid-end/lowering changes do not touch this shape (the surviving guard is a `sadd_overflow`, and the new upstream flag-forwarding covers only `uadd`/`umul`/`smul`; see I-165). Consistently, Ryo's absolute time is unchanged (~362.5 ms vs ~360.8 ms in the 2026-08-26 checkpoint). The ratio improvement to 1.33× is entirely Rust measuring slower this run (~272 ms vs ~253 ms) along with a machine-wide drift (Swift, Go, and Kotlin all moved the same direction); Ryo AOT max resident (1.34 MB) still holds below Rust's (1.45 MB).

### Checkpoint: value-range guard elision (2026-08-26)

This table is the re-measurement after the value-range guard-elision work landed (see commit `d6aee06`): codegen now seeds a per-function value-range fact map from dominating `if`/`while` comparisons against constants and skips `sadd`/`ssub`/`smul_overflow` guards whose operand bounds make overflow impossible. In the fib hot path, `if n <= 1: return n` proves `n >= 2` on the fall-through, so both `n - 1` and `n - 2` guards are now elided — the per-call hot path drops from 29 to 19 machine instructions on aarch64, matching the unguarded shape; only the outer `fibonacci(n - 1) + fibonacci(n - 2)` addition keeps its overflow guard.

Outcome vs the provisional target (fib(40) AOT ≤ 1.25× Rust): **missed** — Ryo AOT measured ~1.42× Rust (~360.8 ms vs ~253.4 ms; a targeted re-run of just Rust vs Ryo confirmed 1.40×). Ryo's absolute time is roughly unchanged versus the previous same-day baseline (~354.9 ms), so on this host the elided guards — perfectly-predicted not-taken branches — were nearly free; the ratio moved mostly because Rust measured faster. The memory headline holds: Ryo AOT max resident (1.34 MB) stays below Rust's (1.45 MB), the lightest of all languages tested. The remaining checked-arithmetic gap is the surviving outer-add guard, whose unfused `cset` + `tst` + `b.ne` lowering is tracked in `ISSUES.md` (I-165).

See [`../README.md`](../README.md#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.37 MB vs Rust's 1.48 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~5.5 MB).


