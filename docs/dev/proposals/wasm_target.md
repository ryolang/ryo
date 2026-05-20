# WASM Target — Proposal (Deferred)

**Status:** Proposal — Deferred post-v0.1.0 (revival requires a strategic commitment to a second backend)

This document describes the design space for a WebAssembly compilation target. As of 2026-05-20, the spike (see [m8d-cranelift-wasm-spike.md](../../analysis/m8d-cranelift-wasm-spike.md)) showed that **Cranelift does not emit WASM**, so the original "thin plumbing" plan collapsed. Reviving WASM requires building a second backend. This proposal sketches what that would entail and what remains valid.

## Changelog

- **v0.4 (2026-05-20):** Reframed as a deferred proposal. The 2026-05-20 spike confirmed that Cranelift 0.131 has no WASM emission backend — its ISAs are `x86`/`arm64`/`s390x`/`riscv64` plus the Pulley interpreter. `cranelift-wasm` is the *consumer* crate (translates WASM → Cranelift IR for execution by wasmtime/wasmer), not a producer. The "Cranelift-emits-WASM" architecture in v0.2/v0.3 was built on a false premise. New direction: if/when WASM becomes a priority, build a parallel TIR→WASM backend using bytecodealliance's `wasm-encoder` crate (the actual emission layer underneath wasm-bindgen and walrus).
- **v0.3 (2026-05-12):** Switched runtime model from "lean on wasi-libc" (L1) to "bundled Rust allocator + direct WASI imports" (L2). Aligned with M8.1's `ryo_rt` runtime crate. *(Now superseded — the wasm-encoder direction reshapes the runtime story anyway.)*
- **v0.2 (initial):** Cranelift WASM backend assumed. *Replaced in v0.4.*

---

## Why this is deferred

The 2026-05-20 spike (see [m8d-cranelift-wasm-spike.md](../../analysis/m8d-cranelift-wasm-spike.md)) established that:

- `cranelift_codegen::isa::lookup(Triple::from_str("wasm32-wasip1"))` returns `LookupError::Unsupported`.
- Cranelift 0.131's available ISAs are `x86`, `arm64`, `s390x`, `riscv64`, and `pulley` (a portable interpreter). There is no wasm32 backend, with or without feature flags.
- `cranelift-wasm` (last published 0.112.3, no longer co-released with main Cranelift) is described as "Translator from WebAssembly to Cranelift IR" — input direction, not output.
- In the Cranelift ecosystem, Cranelift is a WASM **consumer** (used by wasmtime/wasmer to JIT-execute WASM modules), never a WASM **producer**.

The original "WASM Target Plumbing" milestone was sold as "1 week of plumbing" on top of an existing Cranelift WASM backend. With no such backend, every option for shipping WASM requires meaningful new code. None of them are 1-week scope.

---

## The real cost: a second backend

If WASM is revived, the realistic path is **Option α — `wasm-encoder` parallel codegen**:

- [`wasm-encoder`](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-encoder) (bytecodealliance) is the production-grade low-level WASM module builder. It is the emission layer underneath wasm-bindgen, walrus, and most production WASM toolchains. Mature, idiomatic Rust, well-shaped API (function/section/data builders, encode-to-bytes finalization).
- Ryo would gain a second backend (e.g. `src/codegen_wasm.rs`) parallel to the existing Cranelift backend in [codegen.rs](../../../src/codegen.rs). Both backends consume the same TIR; the dispatch happens at codegen entry.

### Architectural implications

- **Stack machine vs SSA.** Cranelift IR is SSA: `let v = ins().iadd(a, b)`. WASM is a structured stack machine: `local.get a; local.get b; i32.add`. The two backends are different mental models for codegen, not different syntaxes for the same thing. TIR→WASM lowering is its own discipline.
- **No relocatable-object → linker stage.** `wasm-encoder` produces a complete module directly. The current [linker.rs](../../../src/linker.rs) abstraction (`zig cc` linking native objects) doesn't apply on WASM — the pipeline branches earlier into "emit module bytes; done."
- **No JIT story on WASM.** Acceptable; embedding wasmtime to JIT-execute WASM is a separable later decision.
- **Static data layout differs.** String literals currently lower to `.rodata` via Cranelift's `DataDescription`. On WASM they live in a memory data segment, addressed via `wasm-encoder`'s `MemorySection` / `DataSection` builders. Parallel infrastructure.
- **WASI imports replace libc calls.** `print()` on native lowers to a `write()` libc call; on WASM it imports `wasi_snapshot_preview1::fd_write` and writes through a wasm-encoder-declared import.

### Scope estimate

- **Initial backend** for the current language surface (M8c complete: int, bool, float, arithmetic, comparisons, if/else, while, for-range, function calls, returns, read-only strings, top-level `print` via WASI `fd_write`): ~800–1500 LOC. Comparable to the current [codegen.rs](../../../src/codegen.rs).
- **Per future language feature:** roughly 1.5× codegen work going forward — every new `TirTag` needs handling in both backends. Heap allocation (M8.1), fat pointers, ownership intrinsics, structs (M9), enums (M11) — all parallel implementations.
- **CI:** new job that builds programs for WASM, executes under `wasmtime`, asserts behavioral parity with the native backend. New dep on `wasmtime` in the CI environment.
- **Testing:** cross-backend behavioral tests for every language feature. The existing integration suite effectively runs twice (once per backend), with a thin parity-checking wrapper.

### Timeline

At the project's current ~8 hr/week pace, a realistic estimate is **4–6 weeks** for initial parity on the existing language surface, plus ongoing parallel work as new features land. This is a strategic commitment, not a tactical pull-forward.

---

## What revival would look like

If the team decides WASM is worth a second backend:

1. **Confirm the strategic priority.** WASM is on the long-term vision ([spec §1](../../specification.md)), but post-spike it costs roughly the same as building a second compiler backend. That's a real choice, not a free win. Don't revive WASM as a side quest.
2. **New milestone, not a phase.** Replace any references to "M8d Phase A plumbing" with a properly-sized milestone (e.g. "WASM Backend"). Slot it after the language surface is stable (probably post-M27, "Core Language Complete").
3. **Backend abstraction first.** Before writing TIR→WASM, factor the existing TIR→Cranelift backend behind a `Backend` trait so dispatch is explicit. This is the genuine plumbing work — and unlike the original M8d, it is both useful (forces the contract between TIR and codegen to be named) and small (~few hundred LOC).
4. **Spike `wasm-encoder` for real.** Emit a minimal `.wasm` module via `wasm-encoder` (constant function, no imports), validate with `wasm-tools validate`, run under `wasmtime`. Confirm the output is genuinely loadable. ~1 day.
5. **Then design the parallel codegen.** TIR→WASM design doc, scope, phasing, CI integration. That is what the *next* version of this proposal looks like.

### Alternatives considered

- **β — `walrus`.** Higher-level transform library, not really a primary codegen target. Wrong tool for emitting from scratch.
- **γ — Switch to LLVM.** Mature WASM backend, but a backend-replacement decision dwarfs the WASM target by itself. Defer unless a separate decision motivates an LLVM move.
- **δ — Stay deferred.** Current state. Zero engineering. Honest about current capability. Revisit when (a) Cranelift adds WASM emission upstream, or (b) the team explicitly accepts the cost of a second backend.

---

## What stays valid

The following sections describe target tradeoffs that hold under any emission backend. Preserved from earlier versions because they are still useful when revival is on the table.

### Goals (if pursued)

1. `ryo build --target wasm32-wasip1 <file.ryo>` produces a `.wasm` module that runs under `wasmtime`, `wasmer`, and Node.js (via WASI shim).
2. All synchronous Ryo programs through Milestone 27 (Core Language Complete) compile and run on WASM with identical observable behavior to native targets.
3. The implementation does not regress native AOT or JIT pipelines.

### Non-Goals

- Browser target (`wasm32-unknown-unknown` with JS host). Needs a separate binding strategy.
- WASI preview 2 / component model. Wait for stabilization.
- Concurrency, threads, async I/O. Blocked on WasmFX + WASI 0.3 — see [concurrency.md](../concurrency.md).
- JIT execution of WASM (`ryo run --target wasm…`). AOT only.
- Source-level debugging (DWARF-in-WASM). Out of scope.

### Type System Adjustments

| Type | Native | WASM | Action |
|---|---|---|---|
| `int` (default) | i64 | i64 | unchanged — WASM has native i64 |
| `uint` | u64 | u64 | unchanged |
| `f32` / `f64` | as-is | as-is | unchanged |
| `i8` / `u8` / `i16` / `u16` | native | stored as i32, narrowed at boundaries | `wasm-encoder` handles narrowing explicitly |
| `i128` / `u128` | software | software (slower) | acceptable; document |
| pointer-sized | 64-bit | 32-bit | a `ptr_type` distinct from Ryo `int` must be threaded through TIR; see the spec-required `int = i64` rule in [specification.md §4.2](../../specification.md) and the audit discussion in [implementation_roadmap.md](../implementation_roadmap.md) M8.1+ |
| `str` / slice fat pointers | `(ptr: i64, len: i64, cap: i64)` | `(ptr: i32, len: i32, cap: i32)` | layout depends on the `ptr_type`-vs-`int_type` split |

The pointer-width audit motivation outlives the WASM target — it applies to any 32-bit future target (embedded RISC-V, microcontrollers, eventual WASM). If desired, it can land as its own small refactor on its own merits, independent of this proposal.

### Features That Will Not Work on WASM

#### Hard Blocks (no path forward without new WASM proposals)

| Feature | Reason | Unblocks when |
|---|---|---|
| `task.run`, `future[T]`, `.await` | Requires stack switching; standard WASM has no accessible execution stack | WasmFX (Phase 4) reaches stable runtimes |
| Channels (`chan[T]`) | Built on the task runtime | Same as above |
| `task.delay` / timers | Need scheduler suspension points | Same as above |
| Real OS threads / parallelism | Needs `wasi-threads` or `SharedArrayBuffer` | Host-dependent |
| `Mutex` / `RwLock` with blocking | Single-threaded WASM cannot block usefully | Threaded WASM |
| Stack-overflow recovery as a Ryo error | WASM has no guard pages; host traps the module | WasmFX or host trap handlers |

#### Soft Blocks (degraded but possible)

| Feature | Status on WASM |
|---|---|
| Networking (`std.net`) | Not in WASI preview 1. Available in preview 2 (`wasi:sockets`); revisit when the component model stabilizes. |
| Subprocess / `std.process.spawn` | Not in WASI. Will return `Unsupported` at runtime. |
| Filesystem | Works, but limited to **preopened** directories (WASI sandbox). No raw `/`-rooted access. |
| Environment variables | Read-only, host-controlled. |
| Panics with stack traces | No DWARF; only message + `proc_exit`. Backtraces deferred. |
| `unsafe` raw pointers | Pointers are 32-bit indices into linear memory; FFI to native libraries is impossible. WASM imports replace C FFI but need a separate binding model. |
| 128-bit ints | Software-emulated, slower. |
| SIMD | Requires the WASM SIMD proposal; emit only when explicitly enabled. |

---

## References

- Spike outcome (load-bearing): [docs/analysis/m8d-cranelift-wasm-spike.md](../../analysis/m8d-cranelift-wasm-spike.md)
- Spec: [§1](../../specification.md) (Target Domains — mentions Wasm), [§17](../../specification.md) (Tooling), [§19](../../specification.md) (Future Work — WebAssembly Target Details)
- Dev: [concurrency.md](../concurrency.md) — WasmFX future direction for concurrency-on-WASM
- Dev: [arc_optimizer.md](../arc_optimizer.md) — target-agnostic ARC pass remains correct under any backend
- External: [wasm-encoder source](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wasm-encoder), [WASI preview 1](https://github.com/WebAssembly/WASI/tree/main/legacy/preview1), [WasmFX](https://wasmfx.dev/)
