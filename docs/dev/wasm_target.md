# WASM Target Implementation Plan (Draft — v0.3)

**Status:** Design (v0.3)

This document describes the plan for adding a WebAssembly compilation target to the Ryo compiler. It is the actionable counterpart to the long-term WasmFX discussion in [concurrency.md](concurrency.md#future-wasm-target-via-wasmfx).

The plan is intentionally scoped to what is achievable **today** with Cranelift's stable WASM backend and WASI preview 1, and explicitly defers everything that depends on stack switching, threads, or async I/O.

## Changelog

- **v0.3 (2026-05-12):** Switched the runtime model from "lean on wasi-libc" (L1) to **"bundled Rust allocator + direct WASI imports"** (L2). This matches Go's no-external-runtime-dependency model on WASM and is consistent with M8.1's commitment to a Go-style embedded runtime crate (`ryo_rt`). Removes the wasi-libc dependency and shrinks the import surface to 3–6 well-known WASI functions per binary. Reflects the existence of the M8.1 runtime crate, which earlier drafts noted as "not yet existing."
- **v0.2 (initial):** Cranelift WASM backend, wasi-libc allocator, wasm-ld via Zig toolchain. Replaced in v0.3.

---

## Goals

1. `ryo build --target wasm32-wasip1 <file.ryo>` produces a `.wasm` module that runs under `wasmtime`, `wasmer`, and Node.js (via WASI shim).
2. All synchronous Ryo programs through Milestone 27 (Core Language Complete) compile and run on WASM with identical observable behavior to native targets.
3. The implementation does not regress native AOT or JIT pipelines.
4. The target is gated behind a feature flag until validated, so an incomplete WASM backend cannot break `cargo test` for native users.

## Non-Goals (current stage)

- Browser target (`wasm32-unknown-unknown` with JS host). Tracked separately; needs a binding strategy.
- WASI preview 2 / component model. Wait for stabilization.
- Concurrency, threads, async I/O. Blocked on WasmFX + WASI 0.3 — see [concurrency.md](concurrency.md).
- JIT execution of WASM (`ryo run --target wasm…`). AOT only.
- Source-level debugging (DWARF-in-WASM). Out of scope for v0.2.

---

## Current Compiler State (relevant facts)

- Backend: **Cranelift 0.130** (`src/codegen.rs`, ~625 LOC). IR is largely target-agnostic.
- Linker: `zig cc` driver (`src/linker.rs`, `src/toolchain.rs`). Zig ships `wasm-ld` out of the box.
- Pipeline: lex → parse → AST → semantic → UIR → TIR → **Ownership pass (M8.1+)** → Cranelift IR → object → link (`src/pipeline.rs`).
- **Runtime crate (`ryo_rt`)** lands in M8.1 — see [M8.1 design doc](../superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md). It produces three artifacts (`staticlib` for native, `cdylib` for WASM, `rlib` for compiler-internal JIT) and bundles its own allocator so neither libc nor wasi-libc is a hard dependency at the runtime layer.
- Concurrency runtime (Phase 5, Milestones 32–34) is **not yet implemented**, so there is no green-thread code to port. This is the easiest possible moment to add WASM support.

---

## Architecture

### Target Selection

Add a `--target` flag to `ryo build` parsed via `target-lexicon` (already a dependency). Recognized values for v0.2:

| Triple | Status |
|---|---|
| `<host>` (default) | Existing native AOT path |
| `wasm32-wasip1` | New — primary WASM target |
| `wasm32-unknown-unknown` | Stretch — emits a `.wasm` with no syscalls; useful for pure-compute libraries |

Internally, plumb a `TargetSpec` struct from `main.rs` → `pipeline.rs` → `codegen.rs` → `linker.rs`. Replace any implicit host-triple usage in `codegen.rs` with the resolved `TargetSpec`.

### Codegen

Cranelift already supports `wasm32` as an ISA target. Concretely:

1. In `codegen.rs`, replace `cranelift_native::builder()` with a triple-aware builder that returns the `wasm32` ISA when targeting WASM.
2. Pointer width becomes 32 bits. Replace any hardcoded `types::I64` for pointer-sized values with `module.target_config().pointer_type()`. Audit:
   - struct field offsets in `tir.rs` and `uir.rs`
   - any `usize`/`isize` lowering in `sema.rs` / `types.rs`
3. Calling convention: Cranelift emits the standard WASM CC automatically. No ABI work needed for the v0.2 type set.
4. Object output: switch from `cranelift-object` (ELF/Mach-O/COFF) to **`cranelift-object` with the WASM target** — Cranelift produces a relocatable WASM object that `wasm-ld` consumes. Verify this against the current Cranelift version; if WASM object emission lags, fall back to writing a single-module `.wasm` directly via `cranelift-wasm` helpers.

### Linker

Replace `zig cc` with `zig wasm-ld` for the WASM target. Critically, **no `-lc` flag** — the `ryo_rt` artifact brings its own allocator and calls WASI imports directly, so there is no need to link wasi-libc.

```shell
zig wasm-ld -o out.wasm user.o ryo_rt.wasm \
    --no-entry --export=_start
```

Add a `Linker::link_wasm` path in `src/linker.rs` parallel to the existing native linker. The Zig toolchain manager (`src/toolchain.rs`) already downloads Zig, so no new toolchain dependency is introduced.

### Runtime / Stdlib Shims

The `ryo_rt` runtime crate (introduced in M8.1) does the heavy lifting on every target. For WASM specifically, the crate bundles its own allocator and calls WASI imports directly — the **L2 Go-style model** in [M8.1's runtime section](../superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md). This replaces the earlier "lean on wasi-libc" plan (L1) so that Ryo WASM modules have no system-library dependency beyond the WASI host imports they actually use.

| Need | Implementation |
|---|---|
| Program entry | Emit `_start` (WASI convention) instead of `main`. Map Ryo's top-level / `main` to `_start`, returning `()` (host gets exit code 0) or an `i32` exit code. |
| `panic` / abort | Lower panics to a call into a tiny runtime wrapper that writes the message via the imported `fd_write` to fd 2 and calls the imported `proc_exit(1)`. |
| Allocator | Runtime bundles `dlmalloc-rs` (or a similar small allocator) as `#[global_allocator]` under `cfg(target_arch = "wasm32")`. All `ryo_str_alloc` / `ryo_str_free` / future container allocations route through it. **No wasi-libc dependency.** |
| stdout / print | When stdlib `print` lands (Milestone 24), route it to the imported `fd_write` directly. The runtime declares the WASI imports with `#[link(wasm_import_module = "wasi_snapshot_preview1")]`. |
| File I/O | Same model — direct WASI imports (`path_open`, `fd_read`, `fd_write`, `fd_close`) declared as `extern "C"` in the runtime, no C shim. |
| Time / random | Direct imports of `clock_time_get` and `random_get`. |
| Atomics (post-M11 `shared[T]`) | Use Rust's `std::sync::atomic`, which compiles to WASM atomics when the `+atomics` feature is enabled. Single-threaded WASM builds use non-atomic loads/stores; semantics are preserved either way. |

Net: the runtime crate is **the only Rust→WASM artifact** linked into a user binary. The output `.wasm` declares 3–6 host imports (the WASI functions actually used) and no other external dependencies. This is the WASM equivalent of Go's "all you need is `chmod +x` and run."

### Type System Adjustments

| Type | Native | WASM | Action |
|---|---|---|---|
| `int` (default) | i64 | i64 | unchanged — WASM has native i64 |
| `uint` | u64 | u64 | unchanged |
| `f32` / `f64` | as-is | as-is | unchanged |
| `i8` / `u8` / `i16` / `u16` | native | stored as i32, narrowed at boundaries | Cranelift handles automatically; verify bool layout |
| `i128` / `u128` | software | software (slower) | acceptable, document |
| pointer-sized | 64-bit | 32-bit | **Audit required** — see Codegen step 2 |
| `str` / slice fat pointers | `(ptr: u64, len: u64, cap: u64)` | `(ptr: u32, len: u32, cap: u32)` | layout already abstracted via `pointer_type()` if step 2 done correctly. The runtime's `RyoStr` uses `usize` so the Rust source compiles to both layouts. See [M8.1 Cranelift Representation](../superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md). |

### CLI Surface

```
ryo build --target wasm32-wasip1 hello.ryo            # → hello.wasm
ryo build --target wasm32-wasip1 --release hello.ryo
ryo target list                                       # show available targets
```

`ryo run` against a WASM target is **not** implemented in v0.2. Users invoke `wasmtime out.wasm` themselves. A future `ryo run --target wasm…` could shell out to `wasmtime` if it is on `PATH`.

---

## Features That Will Not Work on WASM

This list MUST be reflected in user-facing docs (`docs/installation.md` or a new `docs/targets.md`) when the target ships. It mirrors and extends the table in [concurrency.md](concurrency.md#what-is-explicitly-out-of-scope).

### Hard Blocks (no path forward without new WASM proposals)

| Feature | Reason | Unblocks when |
|---|---|---|
| `task.run`, `future[T]`, `.await` | Requires stack switching; standard WASM has no accessible execution stack | WasmFX (Phase 4) reaches stable runtimes |
| Channels (`chan[T]`) | Built on the task runtime | Same as above |
| `task.delay` / timers | Need scheduler suspension points | Same as above |
| Real OS threads / parallelism | Needs `wasi-threads` or `SharedArrayBuffer` | Host-dependent |
| `Mutex` / `RwLock` with blocking | Single-threaded WASM cannot block usefully | Threaded WASM |
| Stack-overflow recovery as a Ryo error | WASM has no guard pages; host traps the module | WasmFX or host trap handlers |

### Soft Blocks (degraded but possible)

| Feature | Status on WASM |
|---|---|
| Networking (`std.net`) | Not in WASI preview 1. Available in preview 2 (`wasi:sockets`); revisit when the component model stabilizes. |
| Subprocess / `std.process.spawn` | Not in WASI. Will return `Unsupported` at runtime. |
| Filesystem | Works, but limited to **preopened** directories (WASI sandbox). No raw `/`-rooted access. |
| Environment variables | Read-only, host-controlled. |
| Panics with stack traces | No DWARF; only message + `proc_exit`. Backtraces deferred. |
| `unsafe` raw pointers | Pointers are 32-bit indices into linear memory; FFI to native libraries is impossible. WASM imports replace C FFI but need a separate binding model (out of scope v0.2). |
| 128-bit ints | Software-emulated, slower. |
| SIMD | Requires the WASM SIMD proposal; emit only when `--features +simd128` is set. v0.2: disabled. |

### Compile-Time Detection

Programs that use unavailable features should fail **at compile time** with a clear diagnostic, not at runtime. Mechanism (design only — implementation deferred):

- Add a `target` predicate usable in `cfg`-style attributes (mirroring Rust's `#[cfg(target_family = "wasm")]`). The exact syntax is a separate spec proposal.
- The stdlib marks symbols like `task.run`, `std.net.*`, `std.process.spawn` with a `#[unavailable(target = "wasm32-*")]` attribute. The semantic analyzer rejects calls to such symbols when the active target matches.
- Until the attribute system exists, document the gaps in `docs/targets.md` and rely on link-time errors (missing WASI imports) as a backstop.

---

## Phased Implementation

Each phase ends in a green CI run and a demo. Estimates assume the same ~8 hr/week pace as the main roadmap.

### Phase A — Plumbing (1 week)

- Add `--target` CLI flag, `TargetSpec` struct, target-aware ISA builder.
- Audit pointer-width assumptions; replace with `pointer_type()`.
- Feature-gate the WASM path behind `cargo build --features wasm` so partial work cannot break native CI.
- **Demo:** `ryo build --target wasm32-wasip1 trivial.ryo` produces a WASM object file (not yet linked).

### Phase B — Runtime Crate WASM Build + Linking + Hello Exit Code (1–2 weeks)

- Extend `runtime/Cargo.toml` to add `cdylib` to `crate-type` and conditional `dlmalloc` dep under `cfg(target_arch = "wasm32")`.
- Add the bundled allocator module (`runtime/src/alloc_wasm.rs`) with `#[global_allocator]` set to `dlmalloc::GlobalDlmalloc`.
- Declare WASI imports (`fd_write`, `proc_exit`, `clock_time_get`, `random_get`) as `extern "C"` in the runtime under the same cfg gate.
- Extend the compiler's `build.rs` to invoke `cargo build -p ryo_rt --target wasm32-wasip1 --release` alongside the native build; embed both artifacts via `include_bytes!`.
- Implement `Linker::link_wasm` invoking `wasm-ld` via the Zig toolchain. Pass `ryo_rt.wasm` from the embedded blob, **no `-lc`**.
- Map top-level / `main` to `_start`; the runtime provides the WASI exit wrapper.
- **Demo:** `wasmtime hello.wasm; echo $?` returns the value of a Ryo program that evaluates to an integer (Milestone 3 parity, on WASM). Output `.wasm` declares 1–2 WASI imports and no others.

### Phase C — Core Language Coverage (1–2 weeks)

- Run the existing Milestone 4–14 integration tests through the WASM target. Fix any 32-bit pointer or i32/i64 narrowing bugs surfaced.
- Add a `cargo test --features wasm` job that runs the WASM suite under `wasmtime` (CI: install `wasmtime` via release tarball, ~3 MB).
- **Demo:** factorial, fizzbuzz, struct/enum examples from `docs/examples/` all run identically on native and WASM.

### Phase D — Stdlib Plumbing (concurrent with Milestone 24)

- When `print`, file I/O, and core stdlib symbols land, ensure they route through the runtime's WASI imports (not wasi-libc).
- Document gaps in `docs/targets.md`.
- **Demo:** `hello_world.ryo` prints "Hello, world!" under `wasmtime --dir=.`.

### Phase E — Polish

- `ryo run --target wasm32-wasip1` shells out to `wasmtime` if available.
- `ryo target list` / `ryo target add` (matches the proposal in [proposals.md](proposals.md)).
- Size optimization pass: `--release` invokes `wasm-opt` (from `binaryen`) if present.

### Out of Phase (deferred)

- Browser target (needs JS bindings, no `_start`, no WASI).
- Concurrency on WASM — covered by the WasmFX section of [concurrency.md](concurrency.md).
- WASI preview 2 / component model.
- DWARF debug info and source-mapped backtraces.

---

## Risks & Open Questions

1. **Cranelift WASM object emission maturity.** `cranelift-object` targeting `wasm32` may not be a fully supported configuration in 0.130. Mitigation: spike during Phase A; if blocked, fall back to single-module emission via `cranelift-wasm`.
2. **Pointer-width audit completeness.** Any silently-baked 64-bit assumption in `tir.rs` / `uir.rs` will produce subtly wrong code. The M8.1 runtime uses `usize` / `*mut u8` so the runtime side is safe, but compiler-side codegen needs a focused review. Mitigation: run every existing integration test under the WASM target before declaring Phase C done.
3. **Zig bundled `wasm-ld` invocation surface.** Verify that the Zig version pinned by `src/toolchain.rs` exposes `zig wasm-ld` cleanly. If not, document a `wasm-ld` system dependency and fall back gracefully.
4. **Allocator choice.** `dlmalloc-rs` is well-maintained and matches Rust's own `wasm32-unknown-unknown` default, but it adds ~10 KB to the WASM module. For size-critical use cases (sub-10 KB binaries) consider `lol_alloc` or a bump allocator. Decision deferable until benchmarks exist.
5. **Future concurrency divergence.** When the green-thread runtime lands (M32+), a WASM build must produce a clear "feature not available on this target" error rather than silently linking nothing. Phase A's feature-gating gives us the hook to enforce this.
6. **Atomics availability.** `shared[T]`'s atomic refcount operations require the WASM atomics proposal (`+atomics` feature). For Phase B–D, atomics are unused (single-threaded execution). When `shared[T]` lands (post-M11) the WASM build must explicitly enable atomics; alternatively, single-threaded WASM builds can use non-atomic ops since contention is impossible.

---

## References

- Spec: §1 (Target Domains, mentions Wasm), §17 (Implementation Strategy — Cranelift WebAssembly support)
- Dev: [concurrency.md](concurrency.md#future-wasm-target-via-wasmfx) (WasmFX deferral), [compilation_pipeline.md](compilation_pipeline.md), [proposals.md](proposals.md) (cross-compilation CLI)
- Dev: [M8.1 design doc](../superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md) (runtime crate, dual-artifact build, pointer-width handling)
- Dev: [arc_optimizer.md](arc_optimizer.md) (target-agnostic refcount elision; runs on TIR before backend lowering)
- Milestone: Slot between Milestones 27 (Core Language Complete) and 28+; adds no language features. To be linked from `implementation_roadmap.md` once approved.
- External: [Cranelift WASM docs](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift), [WASI preview 1 spec](https://github.com/WebAssembly/WASI/tree/main/legacy/preview1), [`dlmalloc-rs`](https://crates.io/crates/dlmalloc), Go's WASI runtime as conceptual prior art.
