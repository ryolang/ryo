# Fibonacci Benchmarks

## Overview

* **Focus:** Deep function recursion, standard integer arithmetic, and basic execution overhead.
* **Languages compared:** Rust, Go, Swift, Kotlin, Bun (TypeScript), Julia, Elixir, Python, Ruby, and Ryo.
* **What to look for:** the checked-arithmetic policy's worst case — three integer ops per call and nothing else to amortize the guards — plus the lightest memory footprint on the table; the diagnosis and both dated checkpoints are below.

## Why Ryo trails Rust here: checked arithmetic is intentional

The 1.00× Rust baseline is compiled in release mode, where integer overflow **wraps silently** — it pays nothing for safety. Ryo's spec (§18) mandates the opposite: every integer `+`, `-`, `*` is checked and **panics on overflow** (spec §18), so each operation carries one predicted-not-taken branch. On this benchmark — three integer ops per recursive call and nothing else — that is the worst possible case for the policy.

The fair like-for-like is **Swift**, which also traps on overflow and sits at ~1.23× Rust. Ryo's remaining margin over Swift is not semantic but mechanical: Cranelift lowers each surviving `sadd`/`ssub` overflow check to `cset` + `tst` + `b.ne` (~3 extra instructions per op; verified by disassembly) instead of a single branch on the CPU overflow flag. Closing that gap is tracked as compiler work, not accepted as a language cost:

* Value-range guard elision **landed** (commit `d6aee06`) — `if n <= 1: return n` proves `n - 1` / `n - 2` cannot overflow, so those guards are gone; see the [checkpoint 2026-08-26](#checkpoint-value-range-guard-elision-2026-08-26).
* The one surviving guard on the outer addition still lowers unfused; the flag-fusing fix is tracked as **I-165** (`ISSUES.md`), and the [checkpoint 2026-09-29](#checkpoint-cranelift-01361-2026-09-29) covers the latest Cranelift bump.

AOT lands at ~1.37× and JIT at ~1.46× Rust's time; the ~23 ms delta is the compiler's compile-and-startup cost inside the timed `ryo run` invocation, not codegen — both share the same Cranelift codegen (both at `opt_level=speed`).

## Results
Calculating the 40th Fibonacci number recursively (Time taken):

<!-- BENCH:START -->
| Language or Arm | Version | Mean Time | Speed vs Best | Memory | Δ prev |
|---|---|---|---|---|---|
| Rust | 1.99.1 | 278.001 ms | 1.00x | 1.478 MB (-0.3%) | +5.5% |
| Kotlin | 2.4.21 | 296.2 ms | 1.07x slower | 44.669 MB (+0.1%) | +9.8% |
| Go | 1.27.3 | 306.954 ms | 1.10x slower | 3.895 MB (+0.4%) | +4.0% |
| Swift | 6.4 | 357.573 ms | 1.29x slower | 1.609 MB (+0.0%) | +8.5% |
| Ryo (AOT) | 0.1.0-dev.20261022+1012fd4 | 375.421 ms | 1.35x slower | ⭐ 1.352 MB (+0.1%) | +3.9% |
| Ryo (JIT) | 0.1.0-dev.20261022+1012fd4 | 403.351 ms | 1.45x slower | 5.478 MB (-0.7%) | +5.1% |
| Bun (TS) | 1.3.13 | 421.819 ms | 1.52x slower | 27.428 MB (-0.0%) | +4.8% |
| Julia | 1.13.2 | 437.615 ms | 1.57x slower | 193.627 MB (+0.0%) | +5.4% |
| Elixir | 1.20.4 | 958.776 ms | 3.45x slower | 90.241 MB (+0.2%) | +4.6% |
| Python | 3.15.0 | 5388.1 ms | 19.38x slower | 17.206 MB (+7.7%) | +7.8% |
| Ruby | 2.6.10p210 | 6427.91 ms | 23.12x slower | 27.997 MB (-0.0%) | +2.8% |

Measured on Darwin/arm64/Mac15,6, 2026-10-22 (commit 1012fd4, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

### Checkpoint: Cranelift 0.136.1 (2026-09-29)

Re-measurement after the Cranelift pin moved 0.135.1 → 0.136.1 (branch `chore/cranelift-0.136.1`). A disassembly diff of the `fibonacci` hot path across the bump is **byte-identical** — the 0.136.1 mid-end/lowering changes do not touch this shape (the surviving guard is a `sadd_overflow`, and the new upstream flag-forwarding covers only `uadd`/`umul`/`smul`; see I-165). Consistently, Ryo's absolute time is unchanged (~362.5 ms vs ~360.8 ms in the 2026-08-26 checkpoint). The ratio improvement to 1.33× is entirely Rust measuring slower this run (~272 ms vs ~253 ms) along with a machine-wide drift (Swift, Go, and Kotlin all moved the same direction); Ryo AOT max resident (1.34 MB) still holds below Rust's (1.45 MB).

### Checkpoint: value-range guard elision (2026-08-26)

This table is the re-measurement after the value-range guard-elision work landed (see commit `d6aee06`): codegen now seeds a per-function value-range fact map from dominating `if`/`while` comparisons against constants and skips `sadd`/`ssub`/`smul_overflow` guards whose operand bounds make overflow impossible. In the fib hot path, `if n <= 1: return n` proves `n >= 2` on the fall-through, so both `n - 1` and `n - 2` guards are now elided — the per-call hot path drops from 29 to 19 machine instructions on aarch64, matching the unguarded shape; only the outer `fibonacci(n - 1) + fibonacci(n - 2)` addition keeps its overflow guard.

Outcome vs the provisional target (fib(40) AOT ≤ 1.25× Rust): **missed** — Ryo AOT measured ~1.42× Rust (~360.8 ms vs ~253.4 ms; a targeted re-run of just Rust vs Ryo confirmed 1.40×). Ryo's absolute time is roughly unchanged versus the previous same-day baseline (~354.9 ms), so on this host the elided guards — perfectly-predicted not-taken branches — were nearly free; the ratio moved mostly because Rust measured faster. The memory headline holds: Ryo AOT max resident (1.34 MB) stays below Rust's (1.45 MB), the lightest of all languages tested. The remaining checked-arithmetic gap is the surviving outer-add guard, whose unfused `cset` + `tst` + `b.ne` lowering is tracked in `ISSUES.md` (I-165).

See [Why Ryo trails Rust here](#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.35 MB vs Rust's 1.48 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~5.5 MB).


