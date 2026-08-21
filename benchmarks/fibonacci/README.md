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
| **Rust** | 1.97.1 | ~253.8 ms | 1.00x         | 1.45 MB               |
| **Kotlin**| 2.4.10 (java 26.0.2) | ~270.5 ms | 1.07x slower | 44.73 MB     |
| **Go**   | 1.26.6 | ~289.9 ms | 1.14x slower  | 4.25 MB               |
| **Swift**| 6.3.3 | ~317.7 ms | 1.25x slower  | 1.62 MB               |
| **Ryo (AOT)** | 0.1.0 | ~357.4 ms | 1.41x slower | **1.36 MB**           |
| **Ryo (JIT)** | 0.1.0 | ~357.7 ms | 1.41x slower | 4.52 MB               |
| **Bun (TS)**  | 1.3.13 | ~401.4 ms | 1.58x slower | 27.44 MB              |
| **Julia** | 1.12.6 | ~424.1 ms | 1.67x slower | 217.66 MB             |
| **Elixir**    | 1.20.3 | ~911.0 ms | 3.59x slower | 75.27 MB              |
| **Python**| 3.14.4 | ~4.975 s | 19.60x slower  | 19.11 MB               |
| **Ruby** | 4.0.6 | ~5.832 s | 22.98x slower | 18.39 MB              |

*(Measured with `hyperfine` on macOS, Apple M3 Pro, 2026-08-22. Ryo is compiled using `--release`.)*

See [`../README.md`](../README.md#why-ryo-trails-rust-here-checked-arithmetic-is-intentional) for why Ryo currently trails Rust on this benchmark (spec §18 checked arithmetic) and what is planned to close the gap.

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.36 MB vs Rust's 1.45 MB).

Even operating entirely as a JIT script interpreting/compiling source code directly, Ryo's compiler (via Cranelift) maintains an incredibly small memory footprint (~4.5 MB).


