# Fibonacci Benchmarks

These benchmarks compare a standard recursive calculation of `fibonacci(40)` across different languages using `hyperfine`. 

*Note: Ryo's recursive function capability works correctly and is competitive with the natively-compiled languages; see the table below and the note on checked arithmetic in [`../README.md`](../README.md).*

## Prerequisites
Before running the benchmarks, ensure you have the following tools installed and available in your PATH:
- `rustc`
- `go`
- `swiftc`
- `uv` (for Python 3.14)
- `bun`
- `elixir`
- `ruby` (installed with homebrew)
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
| **Rust** | 1.98.0 | ~262.2 ms | 1.00x         | 1.45 MB               |
| **Kotlin**| 2.4.10 (java 26.0.2) | ~268.1 ms | 1.02x slower | 44.62 MB     |
| **Go**   | 1.26.6 | ~290.2 ms | 1.11x slower  | 4.16 MB               |
| **Swift**| 6.3.3 | ~329.9 ms | 1.26x slower  | 1.56 MB               |
| **Ryo (AOT)** | 0.1.0 | ~354.9 ms | 1.35x slower | **1.34 MB**           |
| **Ryo (JIT)** | 0.1.0 | ~348.8 ms | 1.33x slower | 4.81 MB               |
| **Bun (TS)**  | 1.3.13 | ~400.9 ms | 1.53x slower | 27.47 MB              |
| **Julia** | 1.12.6 | ~417.5 ms | 1.59x slower | 213.56 MB             |
| **Elixir**    | 1.20.3 | ~884.6 ms | 3.37x slower | 90.19 MB              |
| **Python**| 3.14.4 | ~4.951 s | 18.88x slower  | 19.02 MB               |
| **Ruby** | 4.0.6 | ~5.812 s | 22.17x slower | 18.33 MB              |

*(Measured with `hyperfine` on macOS, Apple M3 Pro, 2026-08-26. Ryo is compiled using `--release`.)*

See [`../README.md`](../README.md#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.34 MB vs Rust's 1.45 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~4.8 MB).


