# JSON Validate Benchmark

**Focus:** Byte-scanning recursive-descent parsing — the workload shape of a real JSON parser's hot loop. A full JSON grammar validator (objects, arrays, strings with escapes and `\uXXXX`, the complete number grammar, literals, whitespace, nesting) walks a 2.95 MB in-program-generated document of 30,000 records and discards values; 12 validation passes per run. Complements byte_slicing's flat scan with deep call structure, per-byte dispatch, and mutual recursion (`parse_value` ↔ `parse_object` / `parse_array`).

**Why validation and not parsing:** a full JSON parser that produces a value tree is blocked on unimplemented language milestones — enums (M11), pattern matching (M12), list/map collections (M22), error unions (M13), plus file I/O. This PoC therefore validates structure and discards values, returning positions instead of trees. The comparison stays meaningful because the scan/validate loop — byte dispatch, string scanning, number lexing — is where real JSON parsers spend most of their time; value-tree construction is the smaller, allocation-bound share.

**Identical algorithm in all three languages:** the same functions, the same `-1`-on-error sentinel, the same in-program document generation (`str_push` / `String::push_str` / `"".join`), the same assert battery, no JSON libraries anywhere. Data is generated in-program because Ryo has no file I/O yet.

**Languages compared:** Rust, Python, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-23. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux). Each run: generate the 2,947,781-byte document, then validate it 12 times.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 5.0 ms ± 0.3 ms | 1.00x | 5.91 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260923+edae0c6 | 97.0 ms ± 1.3 ms | 19.40x slower | 8.52 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260923+edae0c6 | 119.5 ms ± 3.9 ms | 23.90x slower | 14.41 MB |
| **Python** | 3.14.7 | 2314 ms ± 23 ms | 462.80x slower | 25.30 MB |

Ryo AOT runs **23.9x faster than Python** with ~3x less memory. The 19.4x gap to Rust is far wider than in byte_slicing (1.7x): Rust reaches ~10 GB/s per validation pass while Ryo sits at ~0.5 GB/s. Unlike byte_slicing's flat scan, this workload is a deep call tree — every byte goes through `skip_ws` / `parse_value` dispatch and nested `parse_object` / `parse_array` / `parse_string` / `parse_number` calls, and Cranelift has no inliner, so per-byte call overhead and the §18 checked-arithmetic guards on every position update compound instead of amortizing. The JIT row also carries compiler startup and this branch's IR-dump printing.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
