# Ryo Language Benchmarks

This directory contains various benchmarks used to measure, validate, and compare the performance and memory footprint of the **Ryo** programming language compiler and runtime against other languages.

We target both execution speed and memory efficiency (specifically focusing on Ryo's Ahead-Of-Time AOT compiler via Cranelift and JIT execution).

---

## Benchmark Directory

We maintain self-contained, reproducible benchmarks in separate subdirectories:

### 1. [Fibonacci Benchmark](./fibonacci/)
* **Focus:** Deep function recursion, standard integer arithmetic, and basic execution overhead.
* **Languages compared:** Rust, Go, Swift, Kotlin, Bun (TypeScript), Julia, Elixir, Python, Ruby, and Ryo.
* **Highlights:** Ryo AOT is highly competitive in execution speed and achieves the **lightest memory usage of all languages tested** (1.16 MB max resident size).

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

