**Status:** Complete (Phases 1–5 shipped) — this document now tracks only pending work and future considerations

# Pipeline Alignment with Zig: Remaining Work

The multi-phase plan that moved Ryo's middle-end onto the Zig-style pipeline is **complete**: structured diagnostics (`ryo-core/src/diag.rs`), the deepened `InternPool` (`types.rs`), the UIR/TIR split (`uir.rs`, `tir.rs`, `astgen.rs`, `sema/`), and the lazy worklist Sema driver (`DeclState`: `Unresolved → InProgress → Resolved/Failed`, with cycle detection) have all shipped. The compiler today runs `Lexer → Indent → Parser → AstGen → UIR → Sema → TIR → Ownership → Codegen` — see the root `CLAUDE.md` for the authoritative pipeline map. The original phase-by-phase plan text has been removed; git history preserves it.

## Pending

The shipped pipeline is the *substrate*; the features it exists to unblock are still pending. Each is its own roadmap milestone (see `implementation_roadmap.md`), not part of this plan:

- **Compile-time execution (comptime).** A `comptime` UIR instruction asks Sema to evaluate its body to a value instead of emitting TIR; the result is interned in the pool and substituted at TIR emission. The worklist driver is the prerequisite and is in place; the evaluator on top of it is not.
- **Generics with monomorphization.** A generic UIR body is a template; a call site resolves the decl keyed on `(DeclId, [TypeId])` and emits a fresh TIR per instantiation. Per-function TIR arenas already make duplication a `Tir::clone` away.
- **Inline expansion of closures and calls.** Sema splices the callee's TIR into the caller's with arguments substituted — same mechanism as monomorphization, different trigger.

Infrastructure-only test hooks for these already exist, `cfg(any())`-gated, in `ryo-frontend/src/sema/tests.rs`.

## Deferred Decisions

- **`TypeId` as a typed enum (I-018).** The original design proposed `enum TypeId { Void = 0, Bool = 1, ..., Dynamic(NonZeroU32) }` so primitive matches would be exhaustive at compile time. The enum encoding fought the borrow checker, so `TypeId` shipped as a plain `Copy` newtype and primitive access still goes through `TypeKind` (which *is* exhaustive) and the `pool.int()`-style accessors. A re-attempt is tracked in `ISSUES.md` (I-018); low priority.

## Divergence from Zig

The Zig alignment was a starting point, not a constraint. The pipeline is expected to diverge where Ryo's own features demand a different shape, and to grow stages Zig doesn't have:

- **Already diverged: the Ownership pass** (post-sema, pre-codegen — `ryo-frontend/src/ownership/mod.rs`). Zig has no ownership or borrow analysis; Ryo's move semantics and eager-destruction guarantees require a dedicated flow pass over TIR. See `pl_references/mojo.md`.
- **Optimization steps before codegen.** New passes slot in as TIR→TIR transforms between ownership and codegen. The planned `shared[T]` ARC optimizer (retain/release elision, stack promotion, copy-on-write — see `arc_optimizer.md`) is the first of these; the performance promise of `shared[T]` depends on it.
- Future divergences should be recorded in their own dev docs rather than forced back into the Zig mould.

## Risk Register (still valid)

| Risk | Likelihood | Mitigation |
|---|---|---|
| Single-threaded `VecDeque` worklist and `HashMap`-backed `InternPool` block future parallel Sema | Low now, Medium post-v0.2 | Zig shards its `InternPool` and runs analysis on a thread pool specifically because the worklist is concurrent. Keep `Sema` and `InternPool` behind narrow `&mut` APIs so swapping in sharded structures later is a local change; don't leak internal iterators or `&` references to the dedup map across phase boundaries. |

## Out of Scope

These remain roadmap or future considerations, not part of any shipped phase:

- **Splitting UIR into per-file arenas** (Zig does this for incremental compilation; Ryo doesn't compile incrementally yet).
- **Replacing `String`-backed source storage with a `SourceMap`.** Useful for multi-file diagnostics.
- **Moving `print` to the runtime crate.** It is still a compiler builtin (`ryo-frontend/src/builtins.rs`); with structured diagnostics and TIR in place, it can become a normal type-checked call against an external decl.
- **TIR legalization** (Zig's `Air.Legalize`). Cranelift owns target legalization for us; if a non-Cranelift backend ever lands, an `air::legalize`-style pass becomes the right place for scalarizing vectors and expanding overflow checks.
- **A separate MIR layer.** Cranelift IR plays the role of Zig's per-backend MIR: codegen translates TIR directly into Cranelift IR and Cranelift handles instruction selection, register allocation, and emission. No Ryo-owned MIR unless we drop Cranelift.
- **`Compilation` / `Zcu` split.** Zig separates the overall build from the source-only analysis context. A Ryo program is a single compilation unit with no C interop, so both collapse into the pipeline driver + `InternPool`; the split becomes interesting only alongside C interop or multi-package builds.

## References

- Dev: root `CLAUDE.md` — authoritative map of the shipped pipeline. `pl_references/mojo.md` (ownership pass), `arc_optimizer.md` (planned pre-codegen optimization), `pl_references/zig.md` (upstream design borrowed from).
- Milestone/Roadmap: *Compile-time Execution (comptime)* and *Full Generics System* — [implementation_roadmap.md](implementation_roadmap.md) (Phase 5, v0.2+).
- Issues: [../../ISSUES.md](../../ISSUES.md) — I-018 (`TypeId` enum re-attempt).
- Inspiration: Zig's `src/InternPool.zig` (sidecar `extra` array, single pool for types/strings/values), `src/AstGen.zig` → `src/Sema.zig` separation, `src/Zir.zig` / `src/Air.zig` shapes, `src/Module.zig`'s decl worklist.
