# Many Small Strings Benchmark

**Focus:** Flat-loop alloc/free churn. Builds 500,000 short strings (`int_to_str(i) + "!"`), keeps none — each iteration's buffer is freed at its last use inside the same iteration. Complements `eager_destruction` (which stresses deep-recursion lifetimes) with high-frequency flat-loop churn through `ryo_int_to_str`, `ryo_str_concat`, and `ryo_str_free`.

**Languages compared:** Rust, Swift, and Ryo (AOT vs JIT).

## Benchmarks & Performance Results

Measured on **macOS 26.6.2 on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-09-11. Hyperfine `--warmup 3 --shell=none`; peak RSS via `/usr/bin/time -l` (macOS) or `%M` (Linux).

<!-- BENCH:START -->
<!-- BENCH:END -->

### Checkpoint: SSO + consuming concat (2026-09-14)

Re-measured after the string-runtime rework: `str` is now a tagged 24-byte slot — inline for ≤ 23-byte strings, heap with growth headroom beyond that, static `.rodata` for literals. `int_to_str(i) + "!"` is at most 8 bytes, so the per-iteration string never touches the heap: no allocation, and its free is a no-op on the inline tag.

| Candidate | Version | Mean time | vs fastest | Max RSS |
|---|---|---|---|---|
| **Ryo (AOT)** | 0.1.0-dev.20260914+75d0f1e | 9.6 ms ± 0.4 ms | 1.00x | 1.34 MB |
| **Rust** | 1.98.0 | 10.5 ms ± 0.3 ms | 1.10x slower | 1.50 MB |
| **Swift** | 6.3.3 | 10.6 ms ± 0.3 ms | 1.10x slower | 1.58 MB |
| **Ryo (JIT)** | 0.1.0-dev.20260914+75d0f1e | 11.4 ms ± 0.6 ms | 1.18x slower | 5.02 MB |

Ryo AOT went from 19.0 ms (1.86x behind Rust) to 9.6 ms — now the **fastest arm**, ahead of both Rust (10.5 ms) and Swift (10.6 ms), at the lightest RSS. A same-day re-run on a busier machine confirmed the ranking (Ryo AOT 10.7 ms vs Rust 11.5 ms, Swift 11.8 ms).

## How to Run

Prerequisites: `hyperfine`, `rustc`, `swiftc`. The runner builds the release compiler automatically when it uses the repo default `target/release/ryo` (override with `--ryo-bin PATH` or `$RYO_BIN`).

From the repository root:

```bash
uv run benchmarks/run.py run --benchmarks many_small_strings
```

Add `--dry-run` to build every arm and run only the checksum correctness gate (no hyperfine), or `--languages rust,swift,...` to limit the arms.
