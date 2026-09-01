# Development Notes

This directory contains implementation notes, architectural decisions, and design explorations for the Ryo compiler and runtime. These are **working documents for contributors** — not user-facing documentation.

**Lifecycle:** Every file here should eventually be either absorbed into the [specification](../specification.md), implemented in code, or deleted. This directory should be empty by v1.0.

## File Index

### Compiler Internals & IR

| File | Content | Next Action |
|---|---|---|
| [pipeline_alignment.md](pipeline_alignment.md) | Zig-alignment plan — all phases shipped (UIR/TIR split, InternPool, diagnostics, lazy Sema); tracks pending comptime/generics substrate work, Zig divergence notes, and future considerations | **Implement** the pending features as their own roadmap milestones; **delete** once comptime/generics land |
| [architecture_analysis.md](architecture_analysis.md) | Verified snapshot of the full compiler architecture (data structures per stage, ranked weaknesses) + tiered improvement roadmap; source of a tranche of [ISSUES.md](../../ISSUES.md) entries | **Keep as reference**; implement roadmap tiers per ISSUES.md — refresh or delete when stale |
| [closure_representation.md](closure_representation.md) | Closure memory layout, capture, and calling-convention/ABI design | **Implement** when closures land (v0.2+) — then keep as reference |
| [arc_optimizer.md](arc_optimizer.md) | Swift-style ARC retain/release-elision pass design for `shared[T]` | **Implement** post-M11 alongside `shared[T]` — then keep as reference |
| [copy_elision.md](copy_elision.md) | Copy elision rules: guaranteed (G1-G4), permitted (P1-P4), forbidden cases, algorithm sketch | **Implement** in compiler v0.2+ — then keep as reference |
| [stdlib_optimizations.md](stdlib_optimizations.md) | SSO, copy-on-write, sink-parameter conventions for `str` and collections | **Implement** in stdlib v0.2+ — then keep as reference |
| [dyn_trait.md](dyn_trait.md) | Enum dispatch workaround for v0.1 (no `dyn Trait` yet), vtable explanation | **Delete** when `dyn Trait` is implemented in v0.3+ |
| [cranelift_lessons.md](cranelift_lessons.md) | Hard-won Cranelift findings: C ABI struct-return limits, packed-`u128` runtime ABI, unexpressible call annotations, guard flag-fusion, instruction-count≠walltime | **Keep as reference** — read before optimization work in `ryo-backend` |

### Language Comparison References

| File | Content | Next Action |
|---|---|---|
| [zig_reference.md](zig_reference.md) | Zig compiler/toolchain snapshot — the pipeline (lexer→ZIR→AIR) Ryo's middle-end is modelled on | **Keep** while the compiler tracks Zig's shape |
| [mojo_reference.md](mojo_reference.md) | Mojo language snapshot — inspiration for the ownership pass (borrow-by-default, eager destruction) | **Keep** while the ownership pass tracks Mojo |
| [rust_reference.md](rust_reference.md) | Rust borrowck/`Arc` comparison — the bar for Ryo's ownership pass and `shared[T]` | **Keep** as comparison reference |
| [go_reference.md](go_reference.md) | Go language/toolchain snapshot — inspiration for the concurrency model | **Keep** while the concurrency model tracks Go |
| [memory_model_comparison.md](memory_model_comparison.md) | Side-by-side memory-model comparison of Rust, Mojo, Swift, and Ryo | **Keep** as comparison reference |

### Architecture & Design Decisions

| File | Content | Next Action |
|---|---|---|
| [alpha_scope.md](alpha_scope.md) | Defines the v0.0.x alpha as a delivery slice of v0.1.0 | **Delete** when the alpha closes |
| [design_issues.md](design_issues.md) | Open language-design questions, inconsistencies, and grey areas | **Resolve into spec** item by item — then delete |
| [built_in.md](built_in.md) | Compiler magic types vs stdlib boundary (what the compiler must know vs what's a library) | **Move to spec** §4 (Types) or §14 (Stdlib) — then delete |
| [std.md](std.md) | Rust runtime + Ryo wrapper strategy for stdlib (`std.sys` hidden layer + `std.io` public API) | **Move to spec** §14 (Standard Library) — then delete |
| [std_ext.md](std_ext.md) | Curated Rust crates to wrap for stdlib (serde_json, ureq, regex, chrono, rand) | **Move to spec** §14 or a dedicated stdlib contributor guide — then delete |
| [unsafe.md](unsafe.md) | `kind = "system"` gatekeeper pattern for unsafe blocks in `ryo.toml` | **Move to spec** §17 (FFI & unsafe) — then delete |
| [ryo-view-materialization.md](ryo-view-materialization.md) | View materialization: `str(view)`/`bytes(bview)` shipped (M8.4.1.2/M8.4.2); remaining work — `slice[T]` materialization, `From`/`Materialize` + `Clone` traits, `bytes.copy_into`, E0034 machine-applicable suggestion | **Partially implemented** — tracks pending items only; delete once the pending items ship |

### Concurrency

| File | Content | Next Action |
|---|---|---|
| [concurrency.md](concurrency.md) | v0.4 concurrency implementation plan (corosensei green threads, structured concurrency, system-coroutine FFI router, dispatchers — the Loom/Kotlin proposal is adopted and merged in) | **Implement** at the concurrency milestone (v0.4) — then move model to spec |

### Ecosystem & Tooling

| File | Content | Next Action |
|---|---|---|
| [installation.md](installation.md) | Installation UX: one-line script, `~/.ryo/`, zig auto-download, `ryo upgrade` | **Implement** the installer (missing windows) — then move user-facing parts to `docs/installation.md` and delete |
| [pkg_manager.md](pkg_manager.md) | Package manager design (Cargo + Go Modules hybrid) | **Implement** at the package-manager milestone — then delete |
| [official_pkg.md](official_pkg.md) | Recommended official packages for v0.2 (cli, postgres, dotenv, image) | **Implement** the packages — then delete (packages document themselves) |
| [testing.md](testing.md) | Testing framework gaps: fixtures, mocking, RAII patterns, benchmarks | **Move to spec** testing section or implement — then delete |

### Production & Future Concerns

| File | Content | Next Action |
|---|---|---|
| [considerations.md](considerations.md) | Supply chain security, observability hooks, DWARF debug info, Windows UTF-8 paths | **Move relevant items to spec** as they become actionable — then delete |
| [tensor.md](tensor.md) | DLPack/GPU interop, opaque wrapper pattern for safe FFI to TensorFlow/PyTorch | **Move to spec** when data science features are designed — then delete |

### Roadmap & Proposals

| File | Content | Next Action |
|---|---|---|
| [implementation_roadmap.md](implementation_roadmap.md) | 27 milestones across 6 phases with detailed tasks, test counts, completion dates | **Keep updating** as milestones complete — delete when v1.0 ships |
| [proposals.md](proposals.md) | Future feature designs: generics, iterators, comptime, SIMD, dynamic dispatch, Jupyter kernel | **Move accepted proposals to spec** as they're designed — delete when all are resolved |
| [proposals/wasm_target.md](proposals/wasm_target.md) | WASM compilation target proposal, deferred post-v0.1.0 | **Implement** at the WASM milestone (v0.4) — then delete |
