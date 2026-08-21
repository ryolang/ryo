# Ryo Language Benchmarks

This directory contains various benchmarks used to measure, validate, and compare the performance and memory footprint of the **Ryo** programming language compiler and runtime against other languages.

We target both execution speed and memory efficiency (specifically focusing on Ryo's Ahead-Of-Time AOT compiler via Cranelift and JIT execution).

---

## Benchmark Directory

We maintain self-contained, reproducible benchmarks in separate subdirectories:

### 1. [Fibonacci Benchmark](./fibonacci/)
* **Focus:** Deep function recursion, standard integer arithmetic, and basic execution overhead.
* **Languages compared:** Rust, Go, Swift, Kotlin, Bun (TypeScript), Julia, Elixir, Python, Ruby, and Ryo.
* **Highlights:** Ryo AOT achieves the **lightest memory usage of all languages tested** (1.36 MB max resident size). On execution speed, Ryo currently runs at ~1.41× Rust's time on `fib(40)` — see the note below for why.

#### Why Ryo trails Rust here: checked arithmetic is intentional

The 1.00× Rust baseline is compiled in release mode, where integer overflow **wraps silently** — it pays nothing for safety. Ryo's spec (§18) mandates the opposite: every integer `+`, `-`, `*` is checked and **panics on overflow** (spec §18), so each operation carries one predicted-not-taken branch. On this benchmark — three integer ops per recursive call and nothing else — that is the worst possible case for the policy.

The fair like-for-like is **Swift**, which also traps on overflow and sits at ~1.25× Rust. Ryo's remaining margin over Swift is not semantic but mechanical: Cranelift 0.131 lowers each overflow check to `cset` + `uxtb` + `cbnz` (~3 extra instructions per op) instead of a single branch on the CPU overflow flag. Closing that gap is tracked as compiler work, not accepted as a language cost:

- **I-142** (`ISSUES.md`): value-range guard elision — e.g. `if n <= 1: return n` proves `n - 1` and `n - 2` cannot overflow, so their guards should not be emitted at all. This alone would put Ryo near Rust/Go on this benchmark.
- **I-140 / I-141**: Cranelift upgrade tracking; the flag-fusion lowering improvements are upstream work.

JIT and AOT land at the same ~1.41× because both share the same Cranelift codegen.

### 2. [Eager Destruction Benchmark](./eager_destruction/)
* **Focus:** Eager memory deallocation at last use (Eager Destruction / ASAP Destruction) vs. scope-based (RAII) destruction under deep recursion.
* **Languages compared:** Rust (Scope-Based vs. Manual Drop) and Ryo.
* **Highlights:** Ryo AOT uses nearly **2x less heap memory** than standard Rust and is completely immune to stack overflows under deep recursion because deallocations are automatically and eagerly scheduled *before* nested recursive calls.

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

