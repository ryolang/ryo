# Ryo Language Benchmarks

This directory contains various benchmarks used to measure, validate, and compare the performance and memory footprint of the **Ryo** programming language compiler and runtime against other languages.

**Ryo's performance goal:** beat **Python** on performance and **Rust** on simplicity/ergonomics. These suites therefore serve as *regression guardrails*, not parity targets: Ryo already runs fib(40) roughly **19× faster than Python** with the lightest memory footprint of all languages tested. Chasing the remaining gap to Rust/Swift instruction-by-instruction was tried (Cranelift upgrade, string runtime ABI redesign, value-range guard elision, 2026-08) and produced ~zero walltime change — out-of-order hardware absorbs those wins, and the residual gap is structural (middle-end transforms), so micro-optimization is parked until the language is feature-complete. The durable findings live in [`docs/dev/cranelift_lessons.md`](../docs/dev/cranelift_lessons.md).

We target both execution speed and memory efficiency (specifically focusing on Ryo's Ahead-Of-Time AOT compiler via Cranelift and JIT execution).

**Checkpoint convention:** run the full suite before each release and after merging any change that touches generated-code shape (`ryo-backend/src/codegen/`, the Cranelift pin, ownership sidecar consumption); record results in each benchmark's README so the trend is visible in git history.

---

## Benchmark Directory

We maintain self-contained, reproducible benchmarks in separate subdirectories:

### 1. [Fibonacci Benchmark](./fibonacci/)
* **Focus:** Deep function recursion, standard integer arithmetic, and basic execution overhead.
* **Languages compared:** Rust, Go, Swift, Kotlin, Bun (TypeScript), Julia, Elixir, Python, Ruby, and Ryo.
* **Highlights:** Ryo AOT achieves the **lightest memory usage of all languages tested** (1.34 MB max resident size). On execution speed, Ryo currently runs at ~1.42× Rust's time on `fib(40)` — see the note below for why.

#### Why Ryo trails Rust here: checked arithmetic is intentional

The 1.00× Rust baseline is compiled in release mode, where integer overflow **wraps silently** — it pays nothing for safety. Ryo's spec (§18) mandates the opposite: every integer `+`, `-`, `*` is checked and **panics on overflow** (spec §18), so each operation carries one predicted-not-taken branch. On this benchmark — three integer ops per recursive call and nothing else — that is the worst possible case for the policy.

The fair like-for-like is **Swift**, which also traps on overflow and sits at ~1.26× Rust. Ryo's remaining margin over Swift is not semantic but mechanical: Cranelift 0.135.1 lowers each surviving overflow check to `cset` + `tst` + `b.ne` (~3 extra instructions per op; verified by disassembly) instead of a single branch on the CPU overflow flag. Closing that gap is tracked as compiler work, not accepted as a language cost:

- Value-range guard elision **landed** (commit `d6aee06`, checkpoint 2026-08-26 in [`fibonacci/README.md`](./fibonacci/README.md#checkpoint-value-range-guard-elision-2026-08-26)): `if n <= 1: return n` proves `n - 1` and `n - 2` cannot overflow, so those guards are no longer emitted. Measured walltime change was ~zero — the elided branches were perfectly predicted — and the residual gap is structural (middle-end transforms), not the guards.
- The one surviving guard on the outer `fibonacci(n - 1) + fibonacci(n - 2)` addition lowers to an unfused `cset` + `tst` + `b.ne`; the flag-fusing fix is tracked as **I-165** (`ISSUES.md`). Cranelift itself is pinned and upgraded regularly (0.135.1 at the time of writing).

JIT and AOT land within noise of each other (~1.42–1.43×) because both share the same Cranelift codegen (both at `opt_level=speed`).

### 2. [Eager Destruction Benchmark](./eager_destruction/)
* **Focus:** Eager memory deallocation at last use (Eager Destruction / ASAP Destruction) vs. scope-based (RAII) destruction under deep recursion.
* **Languages compared:** Rust (Scope-Based vs. Manual Drop) and Ryo.
* **Highlights:** Ryo AOT uses nearly **3x less heap memory** than standard Rust and is completely immune to stack overflows under deep recursion because deallocations are automatically and eagerly scheduled *before* nested recursive calls.

### 3. [String Building Benchmark](./string_building/)
* **Focus:** Runtime string ABI + eager destruction — concat over 50,000 iterations; the direct before/after measure for the packed-`u128` runtime ABI.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

### 4. [String Slicing Benchmark](./string_slicing/)
* **Focus:** Zero-copy views — scan a 688 KiB in-program-generated string counting substring matches through `strview` slices, copying and storing nothing.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

### 5. [Mandelbrot Benchmark](./mandelbrot/)
* **Focus:** Float codegen — 401×501 grid, max 80 iterations per pixel; no overflow guards in play, the cleanest Cranelift readout.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

### 6. [Collatz Benchmark](./collatz/)
* **Focus:** Integer loop/branch — total stopping time for seeds 1..1,000,000; a hot flat loop complementing fibonacci's recursion profile.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

### 7. [Doubling Concat Benchmark](./doubling_concat/)
* **Focus:** Runtime allocation strategy — `s = s + s` exponential growth to 16 MiB, stressing `ryo_str_alloc` / `ryo_str_concat`.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

### 8. [Many Small Strings Benchmark](./many_small_strings/)
* **Focus:** Flat-loop alloc/free churn — 500,000 short strings built and dropped, complementing eager_destruction's recursion angle.
* **Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

---

## General Prerequisites

To run these benchmarks, you will need `hyperfine` and a built release binary of the Ryo compiler:

```bash
# Build the Ryo compiler in release mode from the repository root
cargo build --release
```

Each benchmark directory contains its own `run_benchmarks.sh` wrapper script. Navigate to any subdirectory and execute the script:

```bash
cd benchmarks/eager_destruction
./run_benchmarks.sh
```

---

## Profiling with Samply

To deeply inspect the performance characteristics of Ryo's execution via flamegraphs, we recommend using [samply](https://github.com/mstange/samply).

First, install `samply`:
```bash
cargo install samply
```

Then, navigate to any benchmark directory, build the benchmark, and profile either the JIT execution or the standalone AOT compiled binary.

### Example: Profiling Fibonacci

**Profile the standalone AOT binary:**
```bash
samply record ./fib
```

**Profile the JIT compiler executing a file:**
```bash
samply record ../../target/release/ryo run fib.ryo
```

`samply` will execute the provided command and automatically open a browser window displaying an interactive flamegraph, allowing you to trace Cranelift codegen overhead versus actual application execution time.

---

## Manual Checkpoint Convention

Run the full suite (every subdirectory's `run_benchmarks.sh`) **before each release** and **after merging any change that touches generated-code shape** (`ryo-backend/src/codegen/`, the Cranelift pin, ownership sidecar consumption). Record results in each benchmark's README so the trend is visible in git history. These runs are manual only — they never run in CI.
