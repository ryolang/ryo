# JSON Validate Benchmark

**Focus:** Byte-scanning recursive-descent parsing — the workload shape of a real JSON parser's hot loop. A full JSON grammar validator (objects, arrays, strings with escapes and `\uXXXX`, the complete number grammar, literals, whitespace, nesting) walks a 2.95 MB in-program-generated document of 30,000 records and discards values; 12 validation passes per run. Complements byte_slicing's flat scan with deep call structure, per-byte dispatch, and mutual recursion (`parse_value` ↔ `parse_object` / `parse_array`).

**Why validation and not parsing:** a full JSON parser that produces a value tree is blocked on unimplemented language milestones — enums (M11), pattern matching (M12), list/map collections (M22), error unions (M13), plus file I/O. This PoC therefore validates structure and discards values, returning positions instead of trees. The comparison stays meaningful because the scan/validate loop — byte dispatch, string scanning, number lexing — is where real JSON parsers spend most of their time; value-tree construction is the smaller, allocation-bound share.

**Identical algorithm in all five languages:** the same functions, the same `-1`-on-error sentinel, the same in-program document generation (`str_push` / `String::push_str` / `String +=` / `strings.Builder` / `"".join`), the same assert battery, no JSON libraries anywhere. Data is generated in-program because Ryo has no file I/O yet.

**Languages compared:** Rust, Swift, Go, Python, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-23. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux). Each run: generate the 2,947,781-byte document, then validate it 12 times.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Rust** | 1.98.0 | 5.0 ms ± 0.2 ms | 1.00x | 5.91 MB |
| **Go** | 1.27.1 | 40.0 ms ± 0.6 ms | 7.98x slower | 12.31 MB |
| **Swift** | 6.3.3 | 58.0 ms ± 0.9 ms | 11.59x slower | 10.44 MB |
| **Ryo (AOT)** | 0.1.0-dev.20260923 | 100.2 ms ± 3.0 ms | 20.00x slower | 5.69 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260923 | 122.5 ms ± 8.7 ms | 24.46x slower | 11.39 MB |
| **Python** | 3.14.7 | 2358 ms ± 47 ms | 470.91x slower | 25.36 MB |

Ryo AOT runs **23.5x faster than Python** and has the **lightest RSS of all six arms** at 5.69 MB — below Rust's 5.91 MB. That wasn't the original result: the first version of this benchmark used `to_bytes()` (an owned copy), holding two document buffers at the peak for 8.52 MB. The compiler's W0004 lint (`RedundantToBytes`) flagged the benchmark's own `to_bytes()` sites as never-mutated, never-escaping, and switching them to `as_bytes()` — a zero-copy `bytesview` projection of the string, added alongside the lint — removed the copy entirely. Rust's 5.91 MB is likewise one document buffer (`as_bytes()` borrows the `String`), and both sides carry the same doubling-growth capacity overshoot from building the document.

The time gap tells a different story: Rust reaches ~7 GB/s per validation pass while Ryo sits at ~0.4 GB/s — a 20x gap, far wider than byte_slicing's 1.7x. Unlike byte_slicing's flat scan, this workload is a deep call tree — every byte goes through `skip_ws` / `parse_value` dispatch and nested `parse_object` / `parse_array` / `parse_string` / `parse_number` calls, and Cranelift has no inliner, so per-byte call overhead and the §18 checked-arithmetic guards on every position update compound instead of amortizing (tracked as I-185; the unfused-guard half is I-165). Go (8.0x) and Swift (11.6x) sit between — their optimizers inline the hot parse functions but pay bounds checks / UTF-8-view bridging of their own. The JIT row also carries compiler startup and this branch's IR-dump printing.

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`, `go`, `python3`, plus a release build of the compiler (`cargo build --release` from the repository root — the script runs it for you).

```bash
./run_benchmarks.sh
```
