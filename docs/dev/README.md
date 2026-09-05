# Development Notes

This directory contains implementation notes, architectural decisions, and design explorations for the Ryo compiler and runtime. These are **working documents for contributors** — not user-facing documentation.

**Lifecycle:** Every file here should eventually be either absorbed into the [specification](../specification.md), implemented in code, or deleted. This directory should be empty by v1.0. The one permanent exception is the [Scratch](#scratch) section below — `notes.md` is owner-maintained scratch and exempt from this lifecycle.

## File Index

### Compiler Internals & IR

| File | Content | Next Action |
|---|---|---|
| [pipeline_alignment.md](pipeline_alignment.md) | Zig-alignment plan — all phases shipped (UIR/TIR split, InternPool, diagnostics, lazy Sema); tracks pending comptime/generics substrate work, Zig divergence notes, and future considerations | **Implement** the pending features as their own roadmap milestones; **delete** once comptime/generics land |
| [architecture_analysis.md](architecture_analysis.md) | Latest verified snapshot of the full compiler architecture (2026-08-24; data structures per stage, ranked weaknesses) + tiered improvement roadmap; source of a tranche of [ISSUES.md](../../ISSUES.md) entries | **Keep as reference**; implement roadmap tiers per ISSUES.md — refresh or delete when stale (older snapshots live in git history) |
| [closure_representation.md](closure_representation.md) | Closure memory layout, capture, and calling-convention/ABI design | **Implement** when closures land (v0.2+) — then keep as reference |
| [arc_optimizer.md](arc_optimizer.md) | Swift-style ARC retain/release-elision pass design for `shared[T]` | **Implement** post-M11 alongside `shared[T]` — then keep as reference |
| [copy_elision.md](copy_elision.md) | Copy elision rules: guaranteed (G1-G4), permitted (P1-P4), forbidden cases, algorithm sketch | **Implement** in compiler v0.2+ — then keep as reference |
| [stdlib_optimizations.md](stdlib_optimizations.md) | SSO, copy-on-write, sink-parameter conventions for `str` and collections | **Implement** in stdlib v0.2+ — then keep as reference |
| [dyn_trait.md](dyn_trait.md) | Enum dispatch workaround for v0.1 (no `dyn Trait` yet), vtable explanation | **Delete** when `dyn Trait` is implemented in v0.3+ |
| [cranelift_lessons.md](cranelift_lessons.md) | Hard-won Cranelift findings: C ABI struct-return limits, packed-`u128` runtime ABI, unexpressible call annotations, guard flag-fusion, instruction-count≠walltime | **Keep as reference** — read before optimization work in `ryo-backend` |
| [ryo-incremental-compilation.md](ryo-incremental-compilation.md) | Incremental compilation design: L1/L2/L3 staging, interface/body hash split, five "install now" hooks (H-1…H-5) | **Implement** the H-1…H-5 hooks now; stage L1→L3 as roadmap milestones — then keep as reference |

### Language Comparison References

| File | Content | Next Action |
|---|---|---|
| [pl_references/zig.md](pl_references/zig.md) | Zig compiler/toolchain snapshot — the pipeline (lexer→ZIR→AIR) Ryo's middle-end is modelled on | **Keep** while the compiler tracks Zig's shape |
| [pl_references/mojo.md](pl_references/mojo.md) | Mojo language snapshot — inspiration for the ownership pass (borrow-by-default, eager destruction) | **Keep** while the ownership pass tracks Mojo |
| [pl_references/rust.md](pl_references/rust.md) | Rust borrowck/`Arc` comparison — the bar for Ryo's ownership pass and `shared[T]` | **Keep** as comparison reference |
| [pl_references/go.md](pl_references/go.md) | Go language/toolchain snapshot — inspiration for the concurrency model | **Keep** while the concurrency model tracks Go |
| [pl_references/swift.md](pl_references/swift.md) | Swift compiler snapshot (latest release 6.3.3) — SIL ARC optimizer (`lib/SILOptimizer/ARC/`), the model for `shared[T]`'s implicit refcounting and Ryo's planned ARC elision pass | **Keep** while `shared[T]`/ARC-optimizer work tracks Swift |
| [pl_references/memory_model_comparison.md](pl_references/memory_model_comparison.md) | Side-by-side memory-model comparison of Rust, Mojo, Swift, and Ryo | **Keep** as comparison reference |

### Architecture & Design Decisions

| File | Content | Next Action |
|---|---|---|
| [alpha_scope.md](alpha_scope.md) | Defines the v0.0.x alpha as a delivery slice of v0.1.0 | **Delete** when the alpha closes |
| [design_issues.md](design_issues.md) | Open language-design questions, inconsistencies, and grey areas | **Resolve into spec** item by item — then delete |
| [built_in.md](built_in.md) | Compiler-side built-in/stdlib boundary: decision matrix, lang items (`&str`, `Error` layouts), `std.sys` glue layer, `no_std` floor (kept dev-side until runtime profiles are designed) — user-facing content absorbed into spec §4/§14 | **Keep as internals reference** — revisit when runtime profiles land |
| [std.md](std.md) | `std.sys` hidden layer + `std.io` safe facade pattern and build sequencing (internals); hybrid architecture absorbed into spec §14 | **Keep as reference** — delete when `std.io` lands and the pattern is embodied in code |
| [std_ext.md](std_ext.md) | Curated Rust crates to wrap for stdlib (serde_json, ureq, regex, chrono, rand) | **Move to spec** §14 or a dedicated stdlib contributor guide — then delete |
| [unsafe.md](unsafe.md) | FFI binding-author internals: type mapping (opaque pointers, callbacks, string helpers); the unsafe operation set and gating policy are absorbed into spec §17 | **Keep until v0.2** unsafe policy ships — then delete |
| [ryo-view-materialization.md](ryo-view-materialization.md) | View materialization: `str(view)`/`bytes(bview)` shipped (M8.4.1.2/M8.4.2); remaining work — `slice[T]` materialization, `From`/`Materialize` + `Clone` traits, `bytes.copy_into`, E0034 machine-applicable suggestion | **Partially implemented** — tracks pending items only; delete once the pending items ship |
| [ryo-missing-features-and-gaps.md](ryo-missing-features-and-gaps.md) | Gap register: GAP-1 context propagation, GAP-2 integer overflow (resolved — spec §18), GAP-3 volatile MMIO, GAP-4 packed layout, plus Tier-2 items (`std.mem.arena` PoC, `--allocator=` swap, signals/graceful-shutdown checklist) | **Move Tier-2 items** to the roadmap — then delete once the GAP items land |

### Concurrency

| File | Content | Next Action |
|---|---|---|
| [concurrency.md](concurrency.md) | v0.4 concurrency implementation plan (corosensei green threads, system-coroutine FFI router, dispatchers); observable semantics (channels, memory model, async drop, cancel contract, guard `with` rule) now in spec §9.2.2/§9.2.5/§9.2.6/§9.3.2/§14.5.4 | **Implement** at the concurrency milestone (v0.4) — then delete |

### Ecosystem & Tooling

| File | Content | Next Action |
|---|---|---|
| [pkg_manager.md](pkg_manager.md) | Package manager design (Cargo + Go Modules hybrid) | **Implement** at the package-manager milestone — then delete |
| [official_pkg.md](official_pkg.md) | Recommended official packages for v0.2 (cli, postgres, dotenv, image) | **Implement** the packages — then delete (packages document themselves) |
| [testing.md](testing.md) | Testing framework gaps not in spec §15: mocking/DI under static dispatch, `assert_eq` structural diffs, table-driven tests, Drop-on-panic output capture | **Move to spec** testing section or implement — then delete |

### Production & Future Concerns

| File | Content | Next Action |
|---|---|---|
| [considerations.md](considerations.md) | Supply chain security, observability hooks (DWARF, UTF-8 paths, cross-compilation absorbed into spec §16/§7/§14) | **Move remaining items to spec** as they become actionable — then delete |
| [tensor.md](tensor.md) | DLPack/GPU interop, opaque wrapper pattern for safe FFI to TensorFlow/PyTorch | **Move to spec** when data science features are designed — then delete |

### Roadmap & Proposals

| File | Content | Next Action |
|---|---|---|
| [implementation_roadmap.md](implementation_roadmap.md) | 27 milestones across 6 phases with detailed tasks, test counts, completion dates | **Keep updating** as milestones complete — delete when v1.0 ships |
| [proposals.md](proposals.md) | Future feature designs: generics, iterators, comptime, SIMD, dynamic dispatch, Jupyter kernel | **Move accepted proposals to spec** as they're designed — delete when all are resolved |
| [proposals/wasm_target.md](proposals/wasm_target.md) | WASM compilation target proposal, deferred post-v0.1.0 | **Implement** at the WASM milestone (v0.4) — then delete |
| [ryo-context-and-otel-proposal.md](ryo-context-and-otel-proposal.md) | Ambient request-scoped context (deadlines, cancellation, trace IDs) over the task tree + `std.otel` observability stdlib | **Implement** at the concurrency milestone (v0.4) — then delete |
| [ryo-std-data-proposal.md](ryo-std-data-proposal.md) | Data-layer stdlib proposal: `std.db` (.sqlite/.postgres), `std.pool`, `std.cache`, `std.redis` | **Implement** across the v0.2→v0.4 stdlib milestones — then delete |
| [ryo-proposal-review-issues.md](ryo-proposal-review-issues.md) | Severity-ranked review memo for `std_ext.md`, `tensor.md`, `unsafe.md`, and the concurrency plans; open items L-4, L-5, C-2…C-5 | **Resolve open items** into their target docs — then delete |

### Scratch

| File | Content | Next Action |
|---|---|---|
| [notes.md](notes.md) | Personal scratch TODO and link collection (tool ideas, design resources, concurrency runtime links) | **Keep** — owner-maintained scratch; exempt from the absorb/implement/delete lifecycle |
