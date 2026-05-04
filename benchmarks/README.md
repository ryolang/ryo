# Fibonacci Benchmarks

These benchmarks compare a standard recursive calculation of `fibonacci(40)` across different languages using `hyperfine`. 

*Note: As of now, Ryo's recursive function capability works perfectly and is extremely fast!*

## Prerequisites
Before running the benchmarks, ensure you have the following tools installed and available in your PATH:
- `rustc`
- `go`
- `swiftc`
- `python3`
- `bun`
- `elixir`
- `hyperfine`

You must also have a built Ryo compiler binary. By default, `run_benchmarks.sh` expects the `ryo` binary to be built in release mode at `../../target/release/ryo`. You can ensure this by running `cargo build --release` from the repository root before starting the benchmarks.

## Setup
In the `benchmarks/fibonacci` directory, run:
```bash
./run_benchmarks.sh
```

## Results
Calculating the 40th Fibonacci number recursively (Time taken):

| Language | Mean Time | Speed vs Rust | Memory (Max Resident) |
|----------|-----------|---------------|-----------------------|
| **Rust** | ~273.0 ms | 1.00x         | 1.36 MB               |
| **Go**   | ~303.7 ms | 1.11x slower  | 4.00 MB               |
| **Swift**| ~354.5 ms | 1.30x slower  | 1.72 MB               |
| **Ryo (JIT)** | ~378.4 ms | 1.39x slower | 4.09 MB               |
| **Ryo (AOT)** | ~384.4 ms | 1.41x slower | **1.16 MB**           |
| **Bun (TS)**  | ~420.9 ms | 1.47x slower | 27.55 MB              |
| **Elixir**    | ~932.3 ms | 3.26x slower | 88.06 MB              |
| **Python**| ~17.72 s | ~62x slower   | 8.14 MB               |

*(Measurements executed on macOS, Apple M3 Pro. Ryo is compiled using `--release`)*

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.16 MB vs Rust's 1.36 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~4 MB).

## Profiling with Samply

To deeply inspect the performance characteristics of Ryo's execution via flamegraphs, we recommend using [samply](https://github.com/mstange/samply).

First, install `samply`:
```bash
cargo install samply
```

Then, you can profile either the JIT execution or the standalone AOT compiled binary. 

**Profile the AOT binary:**
```bash
samply record ./fib
```

**Profile the JIT compiler executing a file:**
```bash
samply record ../../target/release/ryo run fib.ryo
```

`samply` will execute the provided command and automatically open a browser window displaying the flamegraph, allowing you to trace Cranelift codegen overhead versus actual application execution time.
