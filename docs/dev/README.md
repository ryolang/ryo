# Development Notes

This directory contains implementation notes, architectural decisions, and design explorations for the Ryo compiler and runtime. These are **working documents for contributors** — not user-facing documentation.

**Lifecycle:** Every file here should eventually be either absorbed into the [specification](../specification.md), implemented in code, or deleted. This directory should be empty by v1.0. The one permanent exception is the [Scratch](#scratch) section below — `NOTES.md` is owner-maintained scratch and exempt from this lifecycle.

## File Index

### Compiler Internals & IR

| File | Content | Next Action |
|---|---|---|
| [pipeline_regression_checks.md](pipeline_regression_checks.md) | Differential, memory, diagnostic, and performance checks for the pipeline migration | **Run and record evidence** before migration acceptance |
| [pipeline_migration.md](pipeline_migration.md) | Current pipeline audit, target Flow IR, and migration steps for ownership, cleanup, generics, comptime, and concurrency | **Implement and verify** the remaining contracts; keep the source audit current |
| [pipeline_alignment.md](pipeline_alignment.md) | Zig-alignment plan — all phases shipped (UIR/TIR split, InternPool, diagnostics, lazy Sema); tracks pending comptime/generics substrate work, Zig divergence notes, and future considerations | **Implement** the pending features as their own roadmap milestones; **delete** once comptime/generics land |
| [spec_closure_representation.md](spec_closure_representation.md) | Closure memory layout, capture, and calling-convention/ABI design | **Implement** when closures land (v0.2+) — then keep as reference |
| [spec_arc_optimizer.md](spec_arc_optimizer.md) | Swift-style ARC retain/release-elision pass design for `shared[T]` | **Implement** post-M11 alongside `shared[T]` — then keep as reference |
| [spec_copy_elision.md](spec_copy_elision.md) | Copy elision rules: guaranteed (G1-G4), permitted (P1-P4), forbidden cases, algorithm sketch | **Implement** in compiler v0.2+ — then keep as reference |
| [pattern_matching.md](pattern_matching.md) | How the compiler delivers spec §4.6/§7.4 `match`: pipeline shape, exhaustiveness, arm typing, move-only bindings + E0133, merge strategy, arm-result ownership transfer, deferral registers | **Keep as reference** — update when M12.5/M12.6 lift the deferrals |
| [spec_stdlib_optimizations.md](spec_stdlib_optimizations.md) | SSO, copy-on-write, sink-parameter conventions for `str` and collections | **Implement** in stdlib v0.2+ — then keep as reference |
| [spec_dyn_trait.md](spec_dyn_trait.md) | Enum dispatch workaround for v0.1 (no `dyn Trait` yet), vtable explanation | **Delete** when `dyn Trait` is implemented in v0.3+ |
| [guide_cranelift_lessons.md](guide_cranelift_lessons.md) | Hard-won Cranelift findings: C ABI struct-return limits, packed-`u128` runtime ABI, unexpressible call annotations, guard flag-fusion, instruction-count≠walltime | **Keep as reference** — read before optimization work in `ryo-backend` |
| [proposal_incremental_compilation.md](proposal_incremental_compilation.md) | Incremental compilation design: L1/L2/L3 staging, interface/body hash split, five "install now" hooks (H-1…H-5) | **Implement** the H-1…H-5 hooks now; stage L1→L3 as roadmap milestones — then keep as reference |
| [guide_llm_instructions.md](guide_llm_instructions.md) | Normative contributor rules (R1–R21) for AI coding agents: data-oriented design, Rust discipline, diagnostics, Cranelift discipline, testing | **Keep** — update as conventions evolve; source-of-truth chain: spec → slicing final spec → record_proposal_review_issues → these instructions |

### Language Comparison References

| File | Content | Next Action |
|---|---|---|
| [pl_references/zig.md](pl_references/zig.md) | Zig compiler/toolchain snapshot — the pipeline (lexer→ZIR→AIR) Ryo's middle-end is modelled on | **Keep** while the compiler tracks Zig's shape |
| [pl_references/mojo.md](pl_references/mojo.md) | Mojo language snapshot — inspiration for the ownership pass (borrow-by-default, eager destruction) | **Keep** while the ownership pass tracks Mojo |
| [pl_references/hylo.md](pl_references/hylo.md) | Hylo projections and access modes — reference for scope-locked views and owner protection | **Keep** as a projection-model reference |
| [pl_references/rust.md](pl_references/rust.md) | Rust borrowck/`Arc` comparison — the bar for Ryo's ownership pass and `shared[T]` | **Keep** as comparison reference |
| [pl_references/go.md](pl_references/go.md) | Go language/toolchain snapshot — inspiration for the concurrency model | **Keep** while the concurrency model tracks Go |
| [pl_references/swift.md](pl_references/swift.md) | Swift compiler snapshot (latest release 6.4.2) — SIL ARC optimizer (`lib/SILOptimizer/ARC/`), nonescapable references, and unique heap ownership | **Keep** while `shared[T]`/ARC-optimizer work tracks Swift |
| [pl_references/memory_model_comparison.md](pl_references/memory_model_comparison.md) | Side-by-side memory-model comparison of Rust, Mojo, Hylo, Swift, and Ryo | **Keep** as comparison reference |

### Architecture & Design Decisions

| File | Content | Next Action |
|---|---|---|
| [alpha_scope.md](alpha_scope.md) | Defines the v0.0.x alpha as a delivery slice of v0.1.0 | **Delete** when the alpha closes |
| [design_issues.md](design_issues.md) | Flaws, contradictions, and pending decisions in the existing spec | **Resolve into spec** item by item — then delete |
| [spec_builtins.md](spec_builtins.md) | Compiler-side built-in/stdlib boundary: decision matrix, lang items (`&str`, `Error` layouts), `std.sys` glue layer, `no_std` floor (kept dev-side until runtime profiles are designed) — user-facing content absorbed into spec §4/§14 | **Keep as internals reference** — revisit when runtime profiles land |
| [spec_std.md](spec_std.md) | `std.sys` hidden layer + `std.io` safe facade pattern and build sequencing (internals); hybrid architecture absorbed into spec §14 | **Keep as reference** — delete when `std.io` lands and the pattern is embodied in code |
| [spec_std_extensions.md](spec_std_extensions.md) | Curated Rust crates to wrap for stdlib (serde_json, ureq, regex, chrono, rand) | **Move to spec** §14 or a dedicated stdlib contributor guide — then delete |
| [spec_unsafe.md](spec_unsafe.md) | FFI binding-author internals: type mapping (opaque pointers, callbacks, string helpers); the unsafe operation set and gating policy are absorbed into spec §17 | **Keep until v0.2** unsafe policy ships — then delete |
| [record_view_materialization.md](record_view_materialization.md) | View materialization: `str(view)`/`bytes(bview)` shipped (M8.4.1.2/M8.4.2); remaining work — `slice[T]` materialization, `From`/`Materialize` + `Clone` traits, `bytes.copy_into`, E0034 machine-applicable suggestion | **Partially implemented** — tracks pending items only; delete once the pending items ship |
| [record_missing_features_and_gaps.md](record_missing_features_and_gaps.md) | Gap register: GAP-1 context propagation, GAP-2 integer overflow (resolved — spec §18), GAP-3 volatile MMIO, GAP-4 packed layout, plus Tier-2 items (`std.mem.arena` PoC, `--allocator=` swap, signals/graceful-shutdown checklist) | **Move Tier-2 items** to the roadmap — then delete once the GAP items land |
| [spec_slicing_and_memory_model.md](spec_slicing_and_memory_model.md) | Slicing/views/memory-model decisions (D1–D11) as a design decision record + specification; amends the base spec, supersedes the slice-projections and binary-pattern-matching proposals | **Move to spec** section by section as the decisions are implemented — then delete |

### Concurrency

| File | Content | Next Action |
|---|---|---|
| [spec_concurrency.md](spec_concurrency.md) | v0.4 concurrency implementation plan (corosensei green threads, system-coroutine FFI router, dispatchers); observable semantics (channels, memory model, async drop, cancel contract, guard `with` rule) now in spec §9.2.2/§9.2.5/§9.2.6/§9.3.2/§14.5.4 | **Implement** at the concurrency milestone (v0.4) — then delete |
| [record_concurrency_poc.md](record_concurrency_poc.md) | Pre-Phase-1 concurrency proof-of-concept gate results — all three platforms (macOS, Linux, Windows) pass; includes the Windows cross-build recipe (`x86_64-pc-windows-gnu` + mingw-w64) | **Keep as reference** for the concurrency milestone (v0.4) — then delete |

### Ecosystem & Tooling

| File | Content | Next Action |
|---|---|---|
| [spec_package_manager.md](spec_package_manager.md) | Package manager design (Cargo + Go Modules hybrid) | **Implement** at the package-manager milestone — then delete |
| [spec_official_packages.md](spec_official_packages.md) | Recommended official packages for v0.2 (cli, postgres, dotenv, image) | **Implement** the packages — then delete (packages document themselves) |
| [spec_testing_framework.md](spec_testing_framework.md) | Testing framework gaps not in spec §15: mocking/DI under static dispatch, `assert_eq` structural diffs, table-driven tests, Drop-on-panic output capture | **Move to spec** testing section or implement — then delete |

### Production & Future Concerns

| File | Content | Next Action |
|---|---|---|
| [considerations.md](considerations.md) | Supply chain security, observability hooks (DWARF, UTF-8 paths, cross-compilation absorbed into spec §16/§7/§14) | **Move remaining items to spec** as they become actionable — then delete |
| [spec_tensor.md](spec_tensor.md) | DLPack/GPU interop, opaque wrapper pattern for safe FFI to TensorFlow/PyTorch | **Move to spec** when data science features are designed — then delete |

### Roadmap & Proposals

| File | Content | Next Action |
|---|---|---|
| [implementation_roadmap.md](implementation_roadmap.md) | 27 milestones across 6 phases with detailed tasks, test counts, completion dates | **Keep updating** as milestones complete — delete when v1.0 ships |
| [proposals.md](proposals.md) | New features, language extensions, and experimental proposals | **Move accepted proposals to spec** as they're designed — delete when all are resolved |
| [proposal_wasm_target.md](proposal_wasm_target.md) | WASM compilation target proposal, deferred post-v0.1.0 | **Implement** at the WASM milestone (v0.4) — then delete |
| [proposal_context_and_otel.md](proposal_context_and_otel.md) | Ambient request-scoped context (deadlines, cancellation, trace IDs) over the task tree + `std.otel` observability stdlib | **Implement** at the concurrency milestone (v0.4) — then delete |
| [proposal_std_data.md](proposal_std_data.md) | Data-layer stdlib proposal: `std.db` (.sqlite/.postgres), `std.pool`, `std.cache`, `std.redis` | **Implement** across the v0.2→v0.4 stdlib milestones — then delete |
| [record_proposal_review_issues.md](record_proposal_review_issues.md) | Severity-ranked review memo for `spec_std_extensions.md`, `spec_tensor.md`, `spec_unsafe.md`, and the concurrency plans; open items L-4, L-5, C-2…C-5 | **Resolve open items** into their target docs — then delete |

### Scratch

| File | Content | Next Action |
|---|---|---|
| [NOTES.md](NOTES.md) | Personal scratch TODO and link collection (tool ideas, design resources, concurrency runtime links) | **Keep** — owner-maintained scratch; exempt from the absorb/implement/delete lifecycle |
