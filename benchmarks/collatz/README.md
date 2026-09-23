# Collatz Benchmark

**Focus:** Integer loop/branch codegen. Sum of Collatz total stopping times for seeds 1..1,000,000 — a hot integer loop with a data-dependent branch and a function call per seed, complementing fibonacci's deep-recursion call profile with a flat iterative one.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 104.3 ms ± 1.2 ms | 1.00x | 1.44 MB |
| **Swift** | 6.3.3 | 164.4 ms ± 1.0 ms | 1.58x slower | 5.55 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260911+b3b7d25 | 216.0 ms ± 1.3 ms | 2.07x slower | 1.36 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260911+b3b7d25 | 220.8 ms ± 2.3 ms | 2.12x slower | 5.09 MB |

## Why Ryo trails Rust here

Disassembly-level diagnosis (2026-09-22: `objdump` diff of the three `collatz_steps` loops plus a control build; ratios reproduce the 2026-09-11 checkpoint). The 2.07x decomposes into ~1.6x the spec §18 checked-arithmetic policy — shared with Swift — and ~1.3x Cranelift aarch64 lowering gaps. JIT and AOT land within noise of each other because both share the same Cranelift codegen.

The control experiment settles the policy question: the 1.00x Rust baseline is compiled with `rustc -O`, where integer overflow **wraps silently** — it does not run Ryo's policy. Recompiling the identical Rust source with `-C overflow-checks=on` lands at 166.5 ms (1.61x), on top of Swift's 164 ms (1.58x, which also traps on overflow). Equally-checked peers cost ~1.6x; Ryo's remaining 216/165 ≈ 1.3x is mechanical, not semantic.

Why fibonacci loses less (1.42x) under the same policy: there the per-call cost is dominated by call/return and recursion overhead, which amortizes the guard bloat. Collatz is the opposite — the loop body is the entire cost, and `n` is a loop-carried serial dependency, so per-iteration bloat hits 1:1.

Per even-path iteration (of 131.4M total loop iterations across the 1M seeds): unchecked Rust is 7 instructions with a branchless `csinc` select (1 conditional branch); checked Rust and Swift are ~8 instructions / 3 branches; Ryo is ~16 instructions / 3 branches plus 2 unconditional. The CLIF Ryo emits is clean (`srem`, `sdiv`, `smul_overflow`, `sadd_overflow`, `brif` — verifiable via `ryo ir --emit clif`); the bloat is all in Cranelift's aarch64 lowering:

1. `srem x, 2` lowers as a full signed-remainder sequence (`lsr`/`add`/`and`/`sub` + `cbz`, 5 instructions) where LLVM emits a single `tst x, #1` — no egraph rule folds `srem x, 2^k == 0` into a bit test.
2. Every checked op materializes the overflow flag into a boolean and branches on it later (`adds` + `cset vs` + `tst` + `b.ne`, 4 instructions where Swift emits `adds` + `b.vs`). `steps += 1` runs on every iteration, so even-path iterations carry 1 checked op (+2 instructions); `3 * n` and `+ 1` execute only on odd-path iterations, which carry 3 checked ops (+6) — the known flag-fusion gap, I-165.
3. `mul x, 3` is not strength-reduced to `add x, x, lsl #1` — 3-cycle latency on the serial `n` chain vs 1.
4. Loop constants (`mov x2, #3`, `mov x2, #1`) are re-materialized per iteration instead of hoisted, and the back edge is an unfused `b` to a separate `cmp`/`b.ne` at the loop top.
5. Minor: `collatz_steps` is never inlined (Cranelift has no inliner), so each of the 1M seeds pays a frame-setting call — Swift doesn't inline either and still hits 164 ms.

Closing the residual ~1.3x is compiler work, not a language cost: the `srem`-by-power-of-two fold, `imul`-by-small-constant strength reduction, and the I-165 flag fusion are all Cranelift/codegen items; a TIR-level inliner would address (5).

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
