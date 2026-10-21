# Fibonacci Benchmarks

## Overview

* **Focus:** Deep function recursion, standard integer arithmetic, and basic execution overhead.
* **Languages compared:** Rust, Go, Swift, Kotlin, Bun (TypeScript), Julia, Elixir, Python, Ruby, and Ryo.
* **Highlights:** Ryo AOT achieves the **lightest memory usage of all languages tested** (1.37 MB max resident size). On execution speed, Ryo currently runs at ~1.34× Rust's time on `fib(40)` — see the note below for why.

## Why Ryo trails Rust here: checked arithmetic is intentional

The 1.00× Rust baseline is compiled in release mode, where integer overflow **wraps silently** — it pays nothing for safety. Ryo's spec (§18) mandates the opposite: every integer `+`, `-`, `*` is checked and **panics on overflow** (spec §18), so each operation carries one predicted-not-taken branch. On this benchmark — three integer ops per recursive call and nothing else — that is the worst possible case for the policy.

The fair like-for-like is **Swift**, which also traps on overflow and sits at ~1.23× Rust. Ryo's remaining margin over Swift is not semantic but mechanical: Cranelift lowers each surviving `sadd`/`ssub` overflow check to `cset` + `tst` + `b.ne` (~3 extra instructions per op; verified by disassembly) instead of a single branch on the CPU overflow flag. Closing that gap is tracked as compiler work, not accepted as a language cost:

* Value-range guard elision **landed** (commit `d6aee06`, [checkpoint 2026-08-26](#checkpoint-value-range-guard-elision-2026-08-26)): `if n <= 1: return n` proves `n - 1` and `n - 2` cannot overflow, so those guards are no longer emitted. Measured walltime change was ~zero — the elided branches were perfectly predicted — and the residual gap is structural (middle-end transforms), not the guards.
* The one surviving guard on the outer `fibonacci(n - 1) + fibonacci(n - 2)` addition lowers to an unfused `cset` + `tst` + `b.ne`; the flag-fusing fix is tracked as **I-165** (`ISSUES.md`). Cranelift itself is pinned and upgraded regularly (0.136.1 at the time of writing; the 0.136.1 upgrade fused `smul_overflow` guards on aarch64 but not yet `sadd`/`ssub`, and left the fib hot path byte-identical — [checkpoint 2026-09-29](#checkpoint-cranelift-01361-2026-09-29)).

AOT lands at ~1.34× and JIT at ~1.46× Rust's time; the ~30 ms delta is the compiler's compile-and-startup cost inside the timed `ryo run` invocation, not codegen — both share the same Cranelift codegen (both at `opt_level=speed`).

These benchmarks compare a standard recursive calculation of `fibonacci(40)` across different languages using `hyperfine`. 

*Note: Ryo's recursive function capability works correctly and is competitive with the natively-compiled languages; see the table below and the note on checked arithmetic [above](#why-ryo-trails-rust-here-checked-arithmetic-is-intentional).*

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

See [Why Ryo trails Rust here](#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.37 MB vs Rust's 1.48 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~5.5 MB).


