**Status:** Reference

# Mojo Programming Language

Last updated on 2026-05-11 (commit [b0032c8](https://github.com/modular/modular/commit/b0032c8), `main`).

[View on GitHub](https://github.com/modular/modular/tree/main/mojo)

> **Caveat — open vs. closed source.** Mojo lives in the `modular/modular` monorepo under `mojo/`. Only the **standard library, proposals, docs, examples, and integration tests** are open source. The Mojo compiler, MLIR dialects (`pop`, `kgen`, `lit`), and runtime (`KGEN_CompilerRT_*`) are closed source. The structural overview below is grounded in what the public repo actually contains; the architecture section also draws on Modular's public talks and the design proposals checked into `mojo/proposals/`.

## Overview

Relevant files

- `mojo/README.md` — landing page
- `mojo/CLAUDE.md` — repo-level contributor and architecture summary
- `mojo/CONTRIBUTING.md` — contribution guide
- `mojo/pixi.toml` / `pixi.lock` — environment and toolchain pins
- `mojo/stdlib/` — Mojo standard library
- `mojo/proposals/` — RFC-style design documents

Mojo is a programming language that combines Python syntax and ecosystem with systems-programming and metaprogramming features. It targets CPUs and GPUs through the Modular Platform and is positioned as a single language for both the high-level scripting layer and the performance-critical kernels underneath.

### Repository Structure

The `mojo/` subdirectory of `modular/modular` is organised as:

- `mojo/stdlib/` — standard library
  - `mojo/stdlib/std/` — source organised by module (`builtin`, `collections`, `memory`, `sys`, `gpu`, …)
  - `mojo/stdlib/test/` — unit tests mirroring the source tree
  - `mojo/stdlib/benchmarks/` — performance benchmarks
  - `mojo/stdlib/scripts/` — build/test wrappers (`run-tests.sh`)
  - `mojo/stdlib/tools/` — auxiliary tooling
  - `mojo/stdlib/docs/` — internal stdlib documentation (style guide, governance)
- `mojo/docs/` — user-facing documentation
  - `mojo/docs/manual/` — Mojo Manual (basics, traits, parameters, GPU, etc.)
  - `mojo/docs/reference/` — language reference material
  - `mojo/docs/releases/` — release notes
- `mojo/examples/` — example Mojo programs
- `mojo/integration-test/` — integration tests
- `mojo/proposals/` — RFC-style design documents
- `mojo/python/` — Python interop layer

### Platform Support

Stated in `mojo/CLAUDE.md`:

- Linux x86\_64 and aarch64
- macOS ARM64
- Windows is **not** currently supported

## Compilation Architecture

Mojo's compiler is closed source; the architecture below is reconstructed from Modular's public talks, the `mojo/proposals/` documents, and the public stdlib APIs that the compiler exposes (e.g. `__mlir_op`, `__mlir_type`).

```mermaid
flowchart LR
    A["Mojo source (.mojo)"] -->|Lex + Parse| B["AST"]
    B -->|Lowering| C["MLIR<br/>(kgen + pop + lit dialects)"]
    C -->|Type checking| D["Typed MLIR"]
    D -->|CheckLifetime pass| E["Ownership-checked MLIR"]
    E -->|Lower| F["LLVM IR"]
    F -->|LLVM| G["Machine code (.mojopkg / binary)"]
```

### MLIR Foundation

Mojo is built on **MLIR**. The compiler maintains three first-party dialects, all internal and not API-stable:

- `kgen` — kernel generation and the bridge to LLVM
- `pop` — high-level Mojo-specific operations
- `lit` — literal and metaprogramming support

Public stdlib code reaches into MLIR via `__mlir_op[...]`, `__mlir_type[...]`, and `__mlir_attr[...]` for primitive operations on built-in types like `Int`, `SIMD`, and `Bool`. See `mojo/stdlib/std/builtin/int.mojo` and `mojo/stdlib/std/builtin/simd.mojo` for examples.

### Output Format

The compiled artifact is a `.mojopkg` package (e.g. `bazel-bin/mojo/stdlib/std/std.mojopkg`), produced by Bazel target `//mojo/stdlib/std`.

## Ownership and Lifetimes

Relevant files

- `mojo/proposals/lifetimes-and-provenance.md` — original lifetimes proposal
- `mojo/proposals/deinit-arg-convention.md` — deinit/owned argument conventions
- `mojo/proposals/copyable-refines-movable.md` — copy/move trait hierarchy
- `mojo/docs/manual/values/` — value semantics in the manual
- `mojo/docs/manual/lifecycle/` — lifecycle / `__del__` documentation

### Value Semantics

Mojo provides full ownership semantics: move, borrow, transfer, mutability, ASAP destruction at last use, and synthesised lifecycle members. Argument conventions are explicit at the call site:

- `borrowed` (default) — immutable borrow
- `inout` — mutable borrow
- `owned` — transfer ownership into the callee

### Origins (Lifetimes)

Mojo has reference types parameterised by an **origin** (formerly "lifetime"). Origins are inferred, not user-written, and are required only for references that escape into return values or struct fields. The compiler pass is called `CheckLifetime` and runs over the typed MLIR; it both diagnoses use-after-move / uninitialised use and inserts destructor calls at last-use points.

### Traits and Metaprogramming

Lifecycle is governed by traits: `Movable`, `Copyable`, `Defaultable`, etc. — defined in `mojo/stdlib/std/builtin/value.mojo`. Compile-time parameters (`parameters` vs. runtime `args`) drive Mojo's metaprogramming and generics system, including parametric `SIMD[dtype, width]` and `InlineArray[T, N]`.

## Standard Library

Relevant files

- `mojo/stdlib/std/__init__.mojo` — module entry point
- `mojo/stdlib/std/prelude/` — auto-imported names
- `mojo/stdlib/std/builtin/` — primitive types
- `mojo/stdlib/std/collections/` — containers
- `mojo/stdlib/std/memory/` — pointer and allocation primitives

The standard library is the canonical reference for what Mojo "is" in the public repo.

### Module Layout

Top-level modules under `mojo/stdlib/std/`:

| Module | Purpose |
|---|---|
| `builtin` | Primitive types (`Int`, `Bool`, `SIMD`, `String`, `Tuple`, `range`, `error`, …) |
| `collections` | `List`, `Dict`, `Set`, `Deque`, `Optional`, `LinkedList`, `InlineArray`, `BitSet` |
| `memory` | `UnsafePointer`, `OwnedPointer`, `ArcPointer`, `Span`, `stack_allocation` |
| `sys` | Target info, intrinsics, libc bindings, HAL, FFI |
| `gpu` | GPU compute primitives, host launch APIs, memory and sync |
| `algorithm` | Vectorised and parallel algorithm primitives |
| `math` | Scalar and SIMD math, polynomial evaluation, constants |
| `hashlib` | Hash functions |
| `io` | Readers, writers, formatted I/O |
| `iter` / `itertools` | Iterator protocol and combinators |
| `os` / `pathlib` / `tempfile` / `subprocess` / `stat` / `pwd` | OS and filesystem |
| `python` | CPython interop |
| `reflection` | Compile-time reflection |
| `runtime` | Async / coroutine runtime |
| `random` | RNG |
| `testing` | Unit-test helpers |
| `benchmark` | Microbenchmark harness |
| `complex` | Complex numbers |
| `compile` | Compile-time introspection |
| `format` | Formatting machinery |
| `time` | Clocks and durations |
| `utils` | Misc. utilities |

### Primitive Types

`mojo/stdlib/std/builtin/` defines the types that the compiler is aware of. Notable files:

- `int.mojo`, `int_literal.mojo`, `float_literal.mojo`, `string_literal.mojo` — literal/numeric primitives
- `simd.mojo` — `SIMD[dtype, size]`, Mojo's core vector type and the backbone of `Float32`, `Int64`, etc.
- `dtype.mojo` — runtime/comptime `DType` descriptors
- `bool.mojo`, `none.mojo`, `tuple.mojo`, `range.mojo`, `error.mojo`
- `value.mojo`, `anytype.mojo`, `comparable.mojo`, `identifiable.mojo`, `floatable.mojo` — lifecycle and capability traits
- `coroutine.mojo` — async coroutine support
- `variadics.mojo` — variadic generics

### Memory Module

`mojo/stdlib/std/memory/` exposes the pointer hierarchy:

- `pointer.mojo` — safe `Pointer`
- `owned_pointer.mojo` — single-owner heap pointer
- `arc_pointer.mojo` — atomic reference-counted pointer
- `unsafe_pointer.mojo` / `unsafe_nullable_pointer.mojo` / `unsafe_maybe_uninit.mojo` — unsafe variants
- `span.mojo` — borrowed contiguous view (the closest analog to Rust's slices)
- `alloc.mojo`, `memory.mojo`, `stack_allocation.mojo` — allocation primitives
- `_poison.mojo` — internal poisoning for use-after-free detection

### Collections

`mojo/stdlib/std/collections/` provides `list.mojo`, `dict.mojo` (backed by `_swisstable.mojo`), `set.mojo`, `deque.mojo`, `linked_list.mojo`, `optional.mojo`, `inline_array.mojo`, `bitset.mojo`, `counter.mojo`, `interval.mojo`, and the `string/` subpackage.

## GPU and Hardware Acceleration

Relevant files

- `mojo/stdlib/std/gpu/` — GPU module
- `mojo/stdlib/std/sys/_hal/` — hardware abstraction layer
- `mojo/stdlib/std/sys/_amdgpu.mojo`, `_metal_print.mojo` — vendor-specific helpers
- `mojo/docs/manual/gpu/` — GPU programming guide

The `gpu` module is split into:

- `gpu/compute/` — kernel-side primitives
- `gpu/host/` — host-side launch APIs
- `gpu/memory/` — device memory management
- `gpu/sync/` — synchronisation primitives
- `gpu/primitives/`, `gpu/intrinsics.mojo`, `gpu/profiler.mojo`

GPU support is a first-class concern in Mojo, not a bolt-on. The same source language is used for host-side orchestration and device-side kernels; the compiler chooses the right MLIR lowering path based on the `@parameter` execution target.

## Build System and Testing

Relevant files

- `mojo/BUILD.bazel` / `mojo/stdlib/std/BUILD.bazel` — Bazel targets
- `mojo/stdlib/scripts/run-tests.sh` — shell wrapper around Bazel
- `mojo/pixi.toml` — pixi environment (toolchain pins)
- `mojo/integration-test/` — integration tests

### Bazel

Build:

```bash
./bazelw build //mojo/stdlib/std
```

Test:

```bash
./bazelw test mojo/stdlib/test/...
./bazelw test //mojo/stdlib/test/memory:test_span.mojo.test
```

`./bazelw run format` formats all Mojo files; the same formatter runs in pre-commit hooks.

### Test Infrastructure

Unit tests live under `mojo/stdlib/test/` and mirror the `std/` source tree. They are driven by `lit` underneath Bazel and default to `-D ASSERT=all`. Integration tests in `mojo/integration-test/` exercise larger end-to-end flows. Benchmarks in `mojo/stdlib/benchmarks/` are required for any performance-claiming change.

### Documentation Validation

```bash
mojo doc --diagnose-missing-doc-strings -Werror -o /dev/null stdlib/std/
```

Docstrings follow the style guide at `mojo/stdlib/docs/docstring-style-guide.md`.

## Proposals Process

Relevant files

- `mojo/proposals/README.md` — proposal index
- `mojo/proposals/lifetimes-and-provenance.md` — origins/lifetimes
- `mojo/proposals/deinit-arg-convention.md` — argument conventions
- `mojo/proposals/copyable-refines-movable.md` — trait refinement
- `mojo/proposals/c-abi-proposal.md` — C interop
- `mojo/proposals/collection-literal-design.md` — literal syntax
- `mojo/proposals/comptime-expr.md` — compile-time expressions
- `mojo/proposals/inferred-parameters.md` — implicit parameter inference
- `mojo/proposals/lifetimes-keyword-renaming.md` — `lifetime` → `origin`
- `mojo/proposals/parameter-binding-syntax.md`
- `mojo/proposals/parametric_alias.md`
- `mojo/proposals/value-ownership.md` — foundational ownership design

Design changes go through markdown RFCs under `mojo/proposals/`. Each proposal carries a status header (`Implemented`, `Partially Implemented`, `Accepted`, `Rejected`, `Draft`). This directory is the most useful single window into Mojo's evolving language design and the closest public approximation of "compiler design notes."

## Internal / Unstable APIs

Per `mojo/CLAUDE.md`, the following carry **no backwards-compatibility guarantee**:

- The MLIR dialects `pop`, `kgen`, `lit` (and any `__mlir_*` directives in stdlib code)
- Compiler runtime entry points prefixed `KGEN_CompilerRT_`
- Anything beginning with `_` in the stdlib

Public stdlib APIs are documented and stable across nightly releases; internal APIs change without notice.

---

## How Ryo Borrows from Mojo

This second half is Ryo-specific context, kept here so the reference doubles as a design-decision log alongside the structural overview above.

Up to milestone M8c the Ryo compiler followed Zig's compiler architecture (lexer → parser → AST → UIR/ZIR → Sema → TIR/AIR → backend). Zig has no borrow checker. M8.1 introduces move semantics, and rather than invent the algorithm from scratch we adopt Mojo's approach — Mojo's lifetime/ownership model is the closest published precedent for what Ryo's spec already says (no annotated lifetimes, no `unsafe` for everyday code, parameters borrow by default).

Ryo borrows Mojo's **implementation strategy** for ownership; it does not borrow Mojo's syntax, type system, or stdlib choices.

### Pipeline Position

Mojo runs ownership checking as MLIR dialect passes that operate on the typed IR *after* the typecheck passes:

```
parse → AST → typecheck (kgen + pop dialects) → CheckLifetime pass → lower → LLVM
```

The ownership pass:

1. Takes already-typed IR.
2. Annotates each SSA value with its ownership state.
3. Inserts destructor calls (`__del__`) at last-use points (ASAP destruction).
4. Reports diagnostics if a value is used after being moved or if an `inout` borrow conflicts.

Ryo's analog: `Sema → TIR → ryo-frontend/src/ownership.rs → Codegen`. The ownership pass mutates the per-function `Tir` in place, inserting `TirTag::Free` instructions and reporting diagnostics through the existing `DiagSink`.

### Argument Conventions

| Mojo | Ryo equivalent | Meaning |
|---|---|---|
| `borrowed x: T` (default) | `x: T` (default) | Immutable borrow; caller keeps the value |
| `inout x: T` | `inout x: T` (M8.2+) | Mutable borrow; caller keeps the value (Ryo adopts the same `inout` spelling) |
| `owned x: T` | `move x: T` | Transfer ownership into the callee |

Ryo's internal data structures use Mojo's terms (`borrowed`/`inout`/`owned`) for clarity, even though the surface syntax is Ryo's own.

### ASAP Destruction

Mojo destroys values at their **last use**, not at scope end. The pass computes last-use points via standard backward liveness analysis on the typed IR. Destructor calls are then inserted at those points. Conditional ownership (a value owned on one branch but not the other) is handled by inserting the destructor on the branch where the value is still owned at the merge.

Ryo spec Section 5.4 ("Hybrid Eager Destruction") promises the same behaviour. M8.1 implements it by inserting `TirTag::Free` instructions as the backward sweep of the ownership pass.

### Origins (formerly "lifetimes")

Mojo recently renamed "lifetimes" to **origins**, parameterising reference types as `Reference[Origin]`. These are *inferred*, not user-written, and they're only needed for **references** — owned values don't carry them.

Ryo's spec Rules 5 and 6 (no returning borrows, no struct fields holding references) sidestep the need for origins entirely in M8.1. Origins may become relevant at M8.4 (`&str` slices), but the M8.1 ownership pass tracks only owned-value state and does not implement origins.

### What we DON'T take from Mojo

- **MLIR.** Ryo uses Cranelift. The ownership pass operates on TIR's flat-arena, per-function representation, not on an MLIR dialect.
- **Comptime metaprogramming.** Mojo has rich metaprogramming for stdlib types; Ryo defers that to v0.2+.
- **`__del__` user hooks.** Ryo's user-extensible drop is M23, not M8.1.
- **Tensor/SIMD types.** Mojo's stdlib leans on tensor/SIMD primitives; that's a separate Ryo concern (see `tensor.md`).
- **Reference syntax.** Mojo's `Reference[T, Origin]` is parametric; Ryo's `&T` (scope-locked view) / `inout T` (mutable parameter convention) (M8.2+) is convention-based and origins are inferred from Rules 5/6.
- **Python interop.** Mojo's `python/` module is core to its positioning; Ryo has no equivalent.
- **GPU as a first-class target.** Ryo targets Cranelift/native and (later) WASM; GPU is out of scope.

## References

- Spec: `docs/specification.md` Section 5 (Memory Management)
- Dev: `docs/dev/borrow_checker.md` (Ryo-specific algorithm sketch)
- Milestone: `docs/dev/implementation_roadmap.md` Milestone 8.1
- Sibling refs: `docs/dev/zig_reference.md`, `docs/dev/go_reference.md`
- Upstream: <https://github.com/modular/modular/tree/main/mojo>, `mojo/CLAUDE.md`, `mojo/proposals/`
