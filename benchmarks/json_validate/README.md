# JSON Validate Benchmark

## Overview

* **Focus:** Byte-scanning recursive-descent parsing — a full JSON grammar validator over a `bytesview` / `&[u8]` / `[UInt8]` / `[]byte` / `bytes` (structure only, values discarded; a value-tree parser is blocked on enums M11, pattern matching M12, collections M22, error unions M13). 30,000-record document generated in-program, then validated 12 times.
* **Languages compared:** Rust, Swift, Go, Python, and Ryo (AOT vs JIT).
* **Highlights:** Ryo AOT runs **22.4x faster than Python** (102 ms vs 2,290 ms) and at 5.69 MB has the **lightest RSS of all six arms** — after the W0004 lint (`RedundantToBytes`) flagged the benchmark's own `to_bytes()` copies and they were switched to the zero-copy `as_bytes()` view. On time it trails Rust by 21x — far wider than byte_slicing's 1.7x on a flat scan: the deep per-byte call tree (no Cranelift inliner) plus §18 checked-arithmetic guards on every position update compound where a flat loop amortized them (tracked as I-188; guard fusion is I-165).

**Focus:** Byte-scanning recursive-descent parsing — the workload shape of a real JSON parser's hot loop. A full JSON grammar validator (objects, arrays, strings with escapes and `\uXXXX`, the complete number grammar, literals, whitespace, nesting) walks a 2.95 MB in-program-generated document of 30,000 records and discards values; 12 validation passes per run. Complements byte_slicing's flat scan with deep call structure, per-byte dispatch, and mutual recursion (`parse_value` ↔ `parse_object` / `parse_array`).

**Why validation and not parsing:** a full JSON parser that produces a value tree is blocked on unimplemented language milestones — enums (M11), pattern matching (M12), list/map collections (M22), error unions (M13), plus file I/O. This PoC therefore validates structure and discards values, returning positions instead of trees. The comparison stays meaningful because the scan/validate loop — byte dispatch, string scanning, number lexing — is where real JSON parsers spend most of their time; value-tree construction is the smaller, allocation-bound share.

**Identical algorithm in all five languages:** the same functions, the same `-1`-on-error sentinel, the same in-program document generation (`str_push` / `String::push_str` / `String +=` / `strings.Builder` / `"".join`), the same assert battery, no JSON libraries anywhere. Data is generated in-program because Ryo has no file I/O yet.

**Languages compared:** Rust, Swift, Go, Python, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Hyperfine `--warmup 3 --shell=none`; peak RSS is hyperfine 2.0's per-run memory measurement, reported as the mean of runs. The table's caption records the machine, date, and compiler commit of the newest measurement. Each run: generate the 2,947,781-byte document, then validate it 12 times.

<!-- BENCH:START -->
<!-- BENCH:END -->

Ryo AOT runs **22.4x faster than Python** and has the **lightest RSS of all six arms** at 5.69 MB — below Rust's 5.91 MB. That wasn't the original result: the first version of this benchmark used `to_bytes()` (an owned copy), holding two document buffers at the peak for 8.52 MB. The compiler's W0004 lint (`RedundantToBytes`) flagged the benchmark's own `to_bytes()` sites as never-mutated, never-escaping, and switching them to `as_bytes()` — a zero-copy `bytesview` projection of the string, added alongside the lint — removed the copy entirely. Rust's 5.91 MB is likewise one document buffer (`as_bytes()` borrows the `String`), and both sides carry the same doubling-growth capacity overshoot from building the document.

The time gap tells a different story: Rust reaches ~7 GB/s per validation pass while Ryo sits at ~0.3 GB/s — a 21x gap, far wider than byte_slicing's 1.7x. Unlike byte_slicing's flat scan, this workload is a deep call tree — every byte goes through `skip_ws` / `parse_value` dispatch and nested `parse_object` / `parse_array` / `parse_string` / `parse_number` calls, and Cranelift has no inliner, so per-byte call overhead and the §18 checked-arithmetic guards on every position update compound instead of amortizing (tracked as I-188; the unfused-guard half is I-165). Swift (6.5x) and Go (7.7x) sit between — their optimizers inline the hot parse functions but pay bounds checks / UTF-8-view bridging of their own. (Swift's earlier 11.6x was partly self-inflicted: `parseValue` built a fresh `Array("true".utf8)` per literal match — hoisting the three literal arrays to file scope, flagged in code review, cut its time from 58.0 ms to 31.8 ms. Go's `[]byte("true")` conversions escaped to the stack under escape analysis, so its gain from the same hoist is smaller; both arms now hoist.) The JIT row also carries compiler startup, and its RSS (11.55 MB) is the whole compiler process — Cranelift, IR arenas — plus the program in one address space; the AOT row is the honest figure for the program's memory behavior.
