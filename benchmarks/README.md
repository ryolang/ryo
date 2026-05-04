# Fibonacci Benchmarks

These benchmarks compare a standard recursive calculation of `fibonacci(40)` across different languages using `hyperfine`. 

*Note: As of now, Ryo's recursive function capability works perfectly and is extremely fast!*

## Prerequisites
Before running the benchmarks, ensure you have the following tools installed and available in your PATH:
- `rustc`
- `go`
- `swiftc`
- `python3`
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
| **Rust** | ~262.5 ms | 1.00x         | 1.36 MB               |
| **Go**   | ~294.3 ms | 1.12x slower  | 3.83 MB               |
| **Swift**| ~330.6 ms | 1.26x slower  | 1.52 MB               |
| **Ryo (JIT)** | ~364.0 ms | 1.39x slower | 4.09 MB               |
| **Ryo (AOT)** | ~362.1 ms | 1.38x slower | **1.16 MB**           |
| **Python**| ~16.89 s | ~64x slower   | 7.73 MB               |

*(Measurements executed on macOS, Apple M3 Pro. Ryo is compiled using `--release`)*

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.16 MB vs Rust's 1.36 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~4 MB).
