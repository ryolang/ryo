**Status:** Reference

# Swift Programming Language

Last updated on 2026-09-01 (commit [cee6e91](https://github.com/swiftlang/swift/commit/cee6e9180ca26d09ef364311516b92c6c61a0a6e), `main`).

Latest release: [Swift 6.3.3](https://github.com/swiftlang/swift/releases/tag/swift-6.3.3-RELEASE) (2026-06-30), the third patch on the [Swift 6.3](https://swift.org/blog/swift-6.3-released/) line (2026-03-24).

[View on GitHub](https://github.com/swiftlang/swift)

## Overview

Relevant files

- `README.md` — landing page
- `CONTRIBUTING.md` — contribution guide
- `CHANGELOG.md` — per-release history (rolling, ~400 KB)
- `docs/` — compiler internals documentation (SIL, ARC, diagnostics, driver)
- `lib/` — compiler implementation (C++)
- `include/` — public compiler headers
- `stdlib/` — standard library and runtime
- `test/`, `validation-test/` — lit-based compiler test suites
- `SwiftCompilerSources/` — compiler passes written in Swift (bootstrapped)
- `utils/` — build scripts (`build-script`, toolchain packaging)

Swift is Apple's general-purpose language, open-sourced in 2015. It combines value semantics with automatic reference counting (ARC) for class types, targets LLVM, and has first-party concurrency (actors, `Sendable`, structured tasks). For Ryo it is the design reference for `shared[T]`'s implicit-refcount model and for the ARC elision pass (see `docs/dev/arc_optimizer.md`).

### Repository Structure

The `swiftlang/swift` monorepo is organised as:

- `lib/` — compiler implementation, one directory per stage:
  - `lib/Parse/` — parser (pure C++; the newer `lib/ASTGen/` is a Swift-syntax-based AST producer)
  - `lib/Sema/` — type checking and constraint solving
  - `lib/SILGen/` — AST → SIL lowering
  - `lib/SIL/` — SIL data structures (the Swift Intermediate Language)
  - `lib/SILOptimizer/` — SIL-to-SIL passes, incl. `lib/SILOptimizer/ARC/` (the ARC optimizer Ryo's planned pass is modelled on)
  - `lib/IRGen/` — SIL → LLVM IR
  - `lib/Driver/`, `lib/Frontend/`, `lib/FrontendTool/` — invocation and pipeline orchestration
  - `lib/ClangImporter/` — C/Objective-C interop import
- `SwiftCompilerSources/` — optimizer passes implemented in Swift itself and bootstrapped into the C++ compiler (including much of the newer ARC/ownership optimization logic)
- `stdlib/public/` — standard library source (`core/`, `Concurrency/`, platform overlays)
- `Runtimes/` — new CMake-native runtime build (core runtime, Concurrency, overlays)
- `tools/` — executables (`swift-frontend`, `swift-driver` shims, `sil-opt`, `swift-demangle`)
- `unittests/` — GoogleTest unit tests; `test/` and `validation-test/` are lit/FileCheck suites

## Compilation Architecture

```mermaid
flowchart LR
    A["Swift source (.swift)"] -->|Parse / ASTGen| B["AST"]
    B -->|Sema (constraint system)| C["Typed AST"]
    C -->|SILGen| D["Raw SIL"]
    D -->|Mandatory passes<br/>(ownership, definite init)| E["Canonical SIL"]
    E -->|SILOptimizer<br/>(incl. ARC opts)| F["Optimized SIL"]
    F -->|IRGen| G["LLVM IR"]
    G -->|LLVM| H["Machine code"]
```

SIL is the distinguishing layer: a Swift-specific SSA IR that sits between the typed AST and LLVM IR, with explicit ownership semantics (`strong_retain`/`strong_release`, `copy_value`/`destroy_value`, ownership-annotated SSA values under "OSSA" — Ownership SSA). Because retain/release operations are visible as ordinary SIL instructions, the optimizer can reason about them directly — this is what makes the ARC optimizer possible as a normal pass pipeline rather than a backend hack.

### The ARC Optimizer (`lib/SILOptimizer/ARC/`)

This is the subsystem Ryo's planned `shared[T]` pass copies. Key components:

- **Retain/release matching** — pairs `strong_retain`/`strong_release` (and `copy_value`/`destroy_value`) that bracket regions where the value is provably live, and removes both.
- **Code motion** — hoists retains and sinks releases to shrink refcounted regions, maximizing the elidable surface.
- **Stack promotion** — converts heap allocations whose lifetime is provably local into stack allocations (the SIL-level analog of escape analysis).
- **Copy-on-write support** — stdlib collections (`Array`, `String`, `Dictionary`) are value types backed by refcounted storage; the optimizer's elision work is what keeps their "copy semantics, reference cost" model performant.

Much of the newer pass logic lives in `SwiftCompilerSources/` (Swift code operating on a bridged SIL representation), so the pass sources come in both C++ and Swift flavors.

### Mandatory vs. Optimization Passes

SIL passes split into two pipelines: **mandatory** passes (definite initialization, ownership verification, `deinit` insertion, diagnostics like capture-of-escaping) that must run even at `-Onone` because they establish correctness and source-level diagnostics, and **optimization** passes (ARC elision, inlining, specialization) that are perf-only and never change program meaning. This mirrors Ryo's split between the soundness ownership pass (`ryo-frontend/src/ownership/mod.rs`) and the planned perf-only ARC optimizer.

## Ownership and Memory Model

- **Value types** (`struct`, `enum`, tuples) — copy semantics; no refcounting when they contain only value data. `Copyable` is the default; `~Copyable` opts out (move-only types, e.g. `File` wrappers that must not be duplicated).
- **Class types** — reference semantics with implicit ARC. Retain/release are compiler-inserted; user code never calls them.
- **Parameter conventions** — `borrowing` (default for most; callee does not consume), `consuming` (callee takes ownership; source invalidated), `inout` (exclusive mutable borrow). `consuming` on a value type enables move semantics.
- **Move semantics** — the compiler diagnoses use-after-`consume` at compile time via the mandatory ownership pass on OSSA SIL.
- **No lifetime annotations** — like Mojo and Ryo, Swift infers everything; there is no `'a` syntax.

## Concurrency

- `actor` types — reference types with built-in mutual exclusion; state is only reachable through `await` suspension points.
- `Sendable` protocol — marker for types safe to share across isolation domains; checked by the compiler.
- Structured concurrency — `Task`, `async let`, task groups; tasks form a tree with cancellation propagation.
- Swift 6 language mode enables full data-race safety checking by default.

## Standard Library

- `stdlib/public/core/` — fundamental types (`Int`, `String`, `Array`, `Optional`), protocols, built-in generics
- `stdlib/public/Concurrency/` — the concurrency runtime library (tasks, actors, executors)
- Copy-on-write is a stdlib-level pattern (`isKnownUniquelyReferenced`), not a language feature — worth studying for Ryo's collections.

## Swift 6.3 (latest minor line)

Per the [release announcement](https://swift.org/blog/swift-6.3-released/) (2026-03-24; current patch 6.3.3):

- **Language:** `@c` attribute to expose Swift functions/enums to C (with optional `@implementation` validation against an existing C header); module selectors (`ModuleA::getValue()`) for disambiguation, including `Swift::` for concurrency/String APIs; library-authoring attributes `@specialize`, `@inline(always)`, `@export(implementation)`.
- **Build:** Swift Build integrated into SwiftPM (preview, unified cross-platform build engine); `swift package show-traits`; prebuilt swift-syntax for macro-only libraries.
- **Testing/DocC:** warning-severity issues, test cancellation, image attachments; experimental Markdown output and code-block annotations in DocC.
- **Platforms:** Embedded Swift improvements (C interop, debugging, linkage model); first official **Swift SDK for Android**.

## How Ryo Borrows from Swift

- **`shared[T]` (spec 5.6)** — modeled on Swift's class reference semantics: refcount ops are implicit on assignment, no `.clone()` in user code. See `docs/dev/pl_references/rust.md` for the explicit Swift-vs-Rust comparison.
- **ARC optimizer** — the pass design in `docs/dev/arc_optimizer.md` (retain/release elision, stack promotion, COW support) is a direct port of the SIL ARC optimizer's responsibilities onto Ryo's TIR. Swift proves the elision-pass investment is what makes implicit refcounting viable.
- **Copy-on-write collections** — stdlib-level COW via uniqueness checking is the model for Ryo's `str`/collections (see `docs/dev/stdlib_optimizations.md`).
- **No lifetime annotations, borrow-by-default conventions** — Swift's `borrowing`/`consuming`/`inout` triad maps to Ryo's default borrow / `move` / `inout` parameter modes (spec 5.2–5.3).

### What we DON'T take from Swift

- **Classes and inheritance.** Ryo's `shared[T]` is a stdlib smart-pointer type over Ryo values, not a separate class hierarchy.
- **Actors.** Ryo's concurrency model is Go-style tasks + channels (see `docs/dev/concurrency.md`), not actor isolation.
- **LLVM-scale pass pipeline.** Ryo's Cranelift backend has no SIL analog; the ARC optimizer runs as a TIR→TIR transform pre-codegen.
- **Stable ABI / library evolution.** Swift pays heavy complexity for ABI stability (`@export`, resilience domains); Ryo is pre-alpha with no ABI commitments.

## References

- Spec: `docs/specification.md` Section 5.6 (Shared Ownership)
- Dev: `docs/dev/arc_optimizer.md` (the planned pass modeled on `lib/SILOptimizer/ARC/`)
- Dev: `docs/dev/pl_references/memory_model_comparison.md` (cross-language memory-model table)
- Dev: `docs/dev/pl_references/rust.md` (comparison against Rust's manual `Arc<T>`)
- Milestone: TBD (sequenced with `shared[T]`) — see `docs/dev/implementation_roadmap.md`
- Upstream: <https://github.com/swiftlang/swift>, `lib/SILOptimizer/ARC/`, `SwiftCompilerSources/`, [Swift 6.3 release notes](https://swift.org/blog/swift-6.3-released/)
