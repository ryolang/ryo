# Eager Destruction & Recursion Benchmark

This benchmark demonstrates and compares **Eager Destruction** (freeing of heap resources at their point of last use) in **Ryo** against scope-based destruction (RAII) in **Rust**.

This is the exact compiler optimization and language feature showcased in Mojo's famous "Mojo vs. Rust: what are the differences?" blog post, where eager destruction is used to optimize memory and enable safer deep recursion without manual cleanups.

---

## The Core Concept

### Rust (Scope-Based RAII)
In Rust, variables are dropped when they go out of scope (at the end of the enclosing block). In a recursive function:
```rust
fn recursive(x: i64) {
    if x == 0 { return; }
    let s = x.to_string(); // Heap allocation occurs here
    if s.len() == 0 { return; }
    recursive(x - 1);
    // <--- Rust's destructor for `s` runs HERE, after recursive returns
}
```
Because the destructor for `s` runs *after* the recursive call, the recursive call `recursive(x - 1)` is not in tail-call position. This results in:
1. **$O(N)$ Peak Heap Memory:** At a recursion depth of $N$, there are $N$ heap-allocated strings concurrently alive in memory because none of them can be freed until the recursion unwinds.
2. **Stack Overflow:** Because the function stack frame must remain active to run the destructor on unwind, Tail Call Optimization (TCO) is inhibited. At deep levels of recursion (e.g., 80,000), Rust crashes with a stack overflow.

### Ryo (Eager Destruction / ASAP Destruction)
In Ryo (completed in **Milestone 8.1**), the compiler's ownership pass performs backward liveness analysis to locate the *last use* of every heap-allocated resource, and schedules its deallocation (`ryo_str_free`) immediately after.
```ryo
fn recursive(x: int):
	if x == 0:
		return
	s: str = int_to_str(x) // Heap allocation occurs here
	if s.len() == 0:       // <--- Last use of `s`
		return
	# Ryo automatically frees `s` here!
	recursive(x - 1)
```
Because the compiler automatically inserts the cleanup call *before* the recursive call:
1. **$O(1)$ Peak Heap Memory:** Only **one** heap-allocated string is alive in memory at any given point, regardless of the recursion depth.
2. **Infinite Stack-Safety / TCO:** The recursive call is in a true tail-call position. No cleanup remains on unwind, allowing the compiler to optimize the stack frames and execute deep recursion (e.g., 80,000 calls) without crashing.

---

## Behind the Scenes: Ryo's Cranelift IR

You can inspect Ryo's generated Cranelift IR to verify that the compiler is actually performing this optimization:
```bash
cargo run -- ir --emit clif benchmarks/eager_destruction/eager_destruction.ryo
```

In the generated Cranelift IR, look at the block where the recursion happens:
```cranelift
block3:
    call fn1(v4, v6)      ; <--- ryo_str_free(ptr, cap) called BEFORE recursion!
    v9 = iconst.i64 1
    v10 = isub.i64 v0, v9
    call fn2(v10)         ; <--- Recursive call is the absolute last operation
    v11 = iconst.i64 0
    return
```
Because `fn1` is called before `fn2`, the string is freed instantly and `fn2` is in tail position!

---

## Benchmarks & Performance Results

To allow direct comparison and capture memory (RSS) metrics across all candidates, the benchmark is configured to run at a recursion depth of **50,000** by default (the limit before Rust's stack frame overhead causes a crash on typical OS configurations).

Measurements executed on **macOS 26.6.2 (Build 25G83) on a MacBook Pro (Apple M3 Pro, 18 GB RAM)**, 2026-08-26, at **50,000** depth (pre-SSO string runtime):

| Benchmark Candidate | Language | Execution Strategy | Max Resident Memory (RSS) | Memory Efficiency (vs Rust Scope-Based) | Result at 50,000 Depth |
|---------------------|----------|--------------------|---------------------------|-------------------|-------------------|
| **Ryo (AOT)** | Ryo 0.1.0 | Standalone Binary (Eager) | **2.86 MB** | **2.90x more efficient** | **Succeeds** |
| **Ryo (JIT)** | Ryo 0.1.0 | JIT Compiler (Eager) | **6.22 MB** | 1.33x more efficient | **Succeeds** |
| **Rust (Manual Drop)** | Rust 1.98.0 | AOT Compiled (Manual `drop(s)`) | **6.80 MB** | 1.22x more efficient | **Succeeds** |
| **Rust (Scope-Based)** | Rust 1.98.0 | AOT Compiled (Scope RAII) | **8.30 MB** | 1.00x (baseline) | **Succeeds** |

### Checkpoint: SSO string runtime (2026-09-15)

Re-measured on the same machine after the SSO + consuming-concat string rework, at `0.1.0-dev.20260914+7be8f41` (hyperfine `--warmup 3 --shell=none`):

| Benchmark Candidate | Max RSS | Memory Efficiency (vs Rust Scope-Based) | Mean time | vs fastest |
|---------------------|---------|------------------------------------------|-----------|------------|
| **Ryo (AOT, Eager)** | **5.11 MB** | **1.62x more efficient** | **2.0 ms ± 0.1 ms** | **1.00x (fastest)** |
| **Ryo (JIT, Eager)** | 8.56 MB | 0.97x | 3.1 ms ± 0.2 ms | 1.54x slower |
| **Rust (Manual Drop)** | 6.80 MB | 1.22x more efficient | 3.0 ms ± 0.1 ms | 1.50x slower |
| **Rust (Scope-Based)** | 8.30 MB | 1.00x (baseline) | 3.2 ms ± 0.1 ms | 1.57x slower |

**Known tradeoff: inline storage vs deep-recursion stack footprint.** The rework changed both numbers above, in opposite directions:

- **Wall time improved.** Strings of ≤ 22 bytes (every `int_to_str` result here is 1–5 chars) now live inline in their 24-byte slot — the per-frame malloc/free pair is gone entirely. CodSpeed's profiler reads instructions **−47%** and CPU cycles **−29%** for this benchmark, and on bare metal Ryo AOT is now the fastest arm of the suite (2.0 ms).
- **RSS grew** (2.86 → 5.11 MB). `int_to_str` is now a slot-out call, so the string slot is address-taken and cannot ride in registers; each recursion frame is ~45 bytes larger, and with 50,000 frames simultaneously live that is ≈ +2.2 MB of materialized stack. Before SSO the per-frame heap block was freed before recursing and the allocator reused one hot block; now the bytes are spread across 50,000 frames. The memory-efficiency lead over Rust scope-based RAII narrows from 2.90x to 1.62x — still ahead, and still O(1) heap.
- **CodSpeed's instrumented wall-time regression (−31%) does not reproduce on bare metal.** Under CodSpeed's memory-mode environment the first-touch cost of the larger stack (memory R/W +81%, cache misses +400% — one cold line per new frame, plus minor page faults on freshly grown stack pages) dominates; hyperfine shows the opposite sign. Both readings are the same trade: strictly less work, spread over a larger footprint, in the one workload shape (50k simultaneously live frames) where that footprint is the cost.

### Key Takeaways
1. **Fastest and leanest-on-heap:** Ryo's AOT binary is the fastest arm of the suite (2.0 ms, 1.50–1.57x over both Rust arms) and keeps O(1) heap — its RSS (5.11 MB) remains below both Rust variants, though SSO's larger stack frames narrowed the margin from 2.90x to 1.62x.
2. **Stack Safety under Deep Recursion:** Rust **crashes with a stack overflow just above 74,556 recursive calls** (re-verified 2026-09-15: depth 74,556 succeeds, 74,600 aborts — both the scope-based and manual-`drop` arms, even with release-level `-O`, due to conservative LLVM tail call heuristics). **Ryo runs completely clean up to ~208,000 recursive calls** (2.8x deeper than Rust) before reaching the OS stack limit. The pre-SSO build reached 260,000; SSO's larger address-taken frames lowered the ceiling, the same tradeoff behind the RSS growth above. The failure modes differ: Rust detects the overflow and aborts cleanly (`thread 'main' has overflowed its stack`, exit 134), while Ryo hits the guard page blind and dies with SIGSEGV (exit 139).
3. **The Power of Compact Stack Frames:** In recursive scope-based RAII, Rust must keep active references, drop flags, and landing pads in each stack frame until the recursion unwinds. By contrast, Ryo's **Milestone 8.1 Eager Destruction** statically releases the string *before* entering recursion — with SSO there is no heap allocation to free at all, and the recursive call is in true tail position.
4. **Observing the Crash:** To observe the stack overflow in Rust and Ryo's stack-safety first-hand, edit the `main()` function in `eager_destruction.ryo` and `eager_destruction.rs` to change `50000` to `74600` (or higher), then re-run `./run_benchmarks.sh`. To see Ryo's own limit, increase its depth past `208000`.

---

## How to Run the Benchmark

### Prerequisites
Make sure you have:
- `rustc`
- `hyperfine`
- A built release compiler binary for Ryo (`cargo build --release` from the repository root)

### Running the Script
Simply run the wrapper script inside this directory:
```bash
./run_benchmarks.sh
```

It will:
1. Build the Rust programs with maximum optimizations (`-O`).
2. Build the Ryo program using AOT.
3. Measure and display the Maximum Resident Set Size (RSS) memory of each.
4. Execute performance comparison runs using `hyperfine`.
