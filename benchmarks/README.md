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
- `ruby`
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
| **Rust** | 1.95.0 | ~265.5 ms | 1.00x         | 1.36 MB               |
| **Kotlin**| 2.3.21 (jvm 26) | ~272.0 ms | 1.02x slower  | 39.50 MB     |
| **Go**   | 1.26.2 | ~297.5 ms | 1.12x slower  | 3.94 MB               |
| **Swift**| 6.2.4 | ~340.0 ms | 1.28x slower  | 1.64 MB               |
| **Ryo (AOT)** | 0.1.0 | ~365.4 ms | 1.38x slower | **1.16 MB**           |
| **Ryo (JIT)** | 0.1.0 | ~369.3 ms | 1.39x slower | 4.20 MB               |
| **Bun (TS)**  | 1.3.13 | ~415.0 ms | 1.56x slower | 27.30 MB              |
| **Julia** | 1.12.6 | ~431.5 ms | 1.63x slower | 216.06 MB             |
| **Elixir**    | 1.19.5 | ~921.8 ms | 3.47x slower | 88.41 MB              |
| **Ruby** | 2.6.10p210 | ~8.97 s | ~33.8x slower | 26.00 MB              |
| **Python**| 3.9.6 | ~17.23 s | ~64.9x slower   | 8.09 MB               |

*(Measurements executed on macOS, Apple M3 Pro. Ryo is compiled using `--release`)*

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.16 MB vs Rust's 1.31 MB).

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
