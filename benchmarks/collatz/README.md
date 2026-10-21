# Collatz Benchmark

## Overview

* **Focus:** Integer loop/branch — total stopping time for seeds 1..1,000,000; a hot flat loop complementing fibonacci's recursion profile.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).
* **Highlights:** Ryo AOT runs at **2.14x Rust** (239.0 ms; measured 2026-10-21), decomposed by disassembly diff into ~1.6x the spec §18 checked-arithmetic policy (equally-checked Rust measures ~1.6x, on par with Swift's 1.63x) plus ~1.3x Cranelift aarch64 lowering gaps (`srem x, 2` not folded to a bit test, unfused `sadd_overflow` branches per I-165 — `smul_overflow` fused in 0.136.1 —, unstrength-reduced `mul x, 3`). See below for the breakdown.

**Focus:** Integer loop/branch codegen. Sum of Collatz total stopping times for seeds 1..1,000,000 — a hot integer loop with a data-dependent branch and a function call per seed, complementing fibonacci's deep-recursion call profile with a flat iterative one.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement.

<!-- BENCH:START -->
| Arm | Time | Peak RSS | Version |
|---|---|---|---|
| Rust | 111.626 ms | 1.463 MB | rustc 1.98.0 |
| Swift | 181.625 ms | 5.576 MB | swiftc 6.4 |
| Ryo (AOT) | 239.027 ms | 1.352 MB | ryo 0.1.0 |
| Ryo (JIT) | 255.785 ms | 5.704 MB | ryo 0.1.0 |
Measured on Darwin/arm64/Mac15,6, 2026-10-21 (commit bddd4e1, hyperfine 2.0.0, warmup 3)
<!-- BENCH:END -->

## Why Ryo trails Rust here

Disassembly-level diagnosis (2026-09-22: `objdump` diff of the three `collatz_steps` loops plus a control build; ratios reproduce the 2026-09-11 checkpoint). The 2.14x decomposes into ~1.6x the spec §18 checked-arithmetic policy — shared with Swift — and ~1.3x Cranelift aarch64 lowering gaps. JIT and AOT land within noise of each other because both share the same Cranelift codegen.

The control experiment settles the policy question: the 1.00x Rust baseline is compiled with `rustc -O`, where integer overflow **wraps silently** — it does not run Ryo's policy. Recompiling the identical Rust source with `-C overflow-checks=on` lands at 166.5 ms (1.61x, 2026-09-11 control build), on top of Swift's 181.6 ms (1.63x, which also traps on overflow). Equally-checked peers cost ~1.6x; Ryo's remaining 239/182 ≈ 1.3x is mechanical, not semantic.

**Checkpoint 2026-09-29 (Cranelift 0.135.1 → 0.136.1):** every candidate measured ~8% slower than on 2026-09-11 (machine-wide drift — Rust, Swift, and Ryo all moved together), so ratios are the honest signal: vs Rust 2.07x → 2.11x, vs Swift 1.31x → 1.33x. The 0.136.1 upgrade did land real instruction-level wins here (see the numbered list below: the `smul_overflow` guard lost its `cset`+`tst`, and the `srem`-by-2 sequence re-formed as `cmp`+`b.eq`), but odd-path iterations are a minority of the 131.4M total, and out-of-order hardware absorbs the rest — consistent with the parked-micro-optimization finding in the suite README. JIT measuring slightly faster than AOT (2.04x vs 2.11x) is run-to-run noise, as before.

Why fibonacci loses less (1.34x) under the same policy: there the per-call cost is dominated by call/return and recursion overhead, which amortizes the guard bloat. Collatz is the opposite — the loop body is the entire cost, and `n` is a loop-carried serial dependency, so per-iteration bloat hits 1:1.

Per even-path iteration (of 131.4M total loop iterations across the 1M seeds): unchecked Rust is 7 instructions with a branchless `csinc` select (1 conditional branch); checked Rust and Swift are ~8 instructions / 3 branches; Ryo is ~14 instructions / 3 branches plus 2 unconditional (was ~16 before the 0.136.1 bump). The CLIF Ryo emits is clean (`srem`, `sdiv`, `smul_overflow`, `sadd_overflow`, `brif` — verifiable via `ryo ir --emit clif`); the bloat is all in Cranelift's aarch64 lowering:

1. `srem x, 2` lowers as a signed-remainder sequence (`lsr`/`add`/`and` + `cmp`/`b.eq`, 5 instructions; `sub`+`cbz` until Cranelift 0.135) where LLVM emits a single `tst x, #1` — no egraph rule folds `srem x, 2^k == 0` into a bit test.
2. Every surviving checked op materializes the overflow flag into a boolean and branches on it later (`adds` + `cset vs` + `tst` + `b.ne`, 4 instructions where Swift emits `adds` + `b.vs`). `steps += 1` runs on every iteration, so even-path iterations carry 1 checked op (+2 instructions); `3 * n` and `+ 1` execute only on odd-path iterations. Cranelift 0.136.1 fused the `smul_overflow` guard (upstream flag-forwarding, bytecodealliance/wasmtime #14228/#14254), so odd-path iterations now carry 2 unfused guards (+4, was +6) — `sadd_overflow`/`ssub_overflow` remain unfused, tracked as I-165.
3. `mul x, 3` is not strength-reduced to `add x, x, lsl #1` — 3-cycle latency on the serial `n` chain vs 1.
4. Loop constants (`mov x2, #3`, `mov x2, #1`) are re-materialized per iteration instead of hoisted, and the back edge is an unfused `b` to a separate `cmp`/`b.ne` at the loop top.
5. Minor: `collatz_steps` is never inlined (Cranelift has no inliner), so each of the 1M seeds pays a frame-setting call — Swift doesn't inline either and still hits 164 ms.

Closing the residual ~1.3x is compiler work, not a language cost: the `srem`-by-power-of-two fold, `imul`-by-small-constant strength reduction, and the I-165 flag fusion are all Cranelift/codegen items; a TIR-level inliner would address (5).
