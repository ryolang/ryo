# Fibonacci Benchmarks

These benchmarks compare a standard recursive calculation of `fibonacci(40)` across different languages using `hyperfine`. 

*Note: As of now, Ryo's recursive function capability works perfectly and is extremely fast!*

## Setup
In the `benchmarks/fibonacci` directory, run:
```bash
./run_benchmarks.sh
```

## Results
Calculating the 40th Fibonacci number recursively (Time taken):

| Language | Mean Time | Speed vs Rust | Memory (Max Resident) |
|----------|-----------|---------------|-----------------------|
| **Rust** | ~262.0 ms | 1.00x         | 1.36 MB               |
| **Go**   | ~296.1 ms | 1.13x slower  | 3.89 MB               |
| **Swift**| ~332.2 ms | 1.27x slower  | 1.64 MB               |
| **Ryo (JIT)** | ~366.4 ms | 1.40x slower | 9.56 MB               |
| **Ryo (AOT)** | ~367.7 ms | 1.40x slower | **1.16 MB**           |
| **Python**| ~16.81 s | ~64x slower   | 8.30 MB               |

*(Measurements executed on macOS)*

### Notes on Memory Usage
Ryo's Ahead-Of-Time (AOT) compiled binary stands out aggressively in memory footprint—claiming the **lightest memory usage of all languages tested** (1.16 MB vs Rust's 1.36 MB).

Ryo's JIT compilation inherently consumes more memory (9.56 MB) due to running the Cranelift compiler infrastructure in memory alongside the actual benchmark logic.
