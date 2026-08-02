# Ryo — Incremental Compilation: Design Note & Roadmap

> **Document Status:** Design Note (Pre-Implementation)
> **Last Updated:** 2026-07-22
> **Version:** 1.0.0-draft
> **Interacts with:** `ryo-compiler-llm-instructions.md` (R4 laziness, R1/R18 DOD), concurrency plan §6.2 (`[yields]` effect inference), base spec §16 (whole-program AOT claims), v0.2 REPL (Cranelift JIT), v0.3 generics/`comptime`
> **Companion philosophy:** Zig compiler (lazy, demand-driven); rustc query model; Go package caching

---

## 1. Why Now

Incremental compilation is nearly free to *enable* at the start and brutally expensive to *retrofit* — rustc spent person-years and two rewrites on it; Zig invested comparably. This document's purpose is therefore **not** to build fine-grained incremental compilation today, but to (a) install the five cheap hooks that keep the door open, (b) stage the real machinery to land when the features that demand it land, and (c) record where Ryo's specific design makes this easy or hard, so no one discovers the hard parts by accident.

## 2. The Three Levels (Scope the Word "Incremental")

| Level | What it is | Prior art | Cost |
|-------|-----------|-----------|------|
| **L1 — Package/module cache** | Unchanged packages are not recompiled; cached objects are relinked | Go (most of why Go feels instant) | Weeks |
| **L2 — Declaration-level reuse** | Only declarations *affected* by an edit are re-analyzed and re-codegen'd | rustc queries, Zig incremental | **Years** — the hardest compiler feature that exists |
| **L3 — In-place binary patching** | Patch changed functions directly into the existing executable; no relink | Zig endgame | Research-grade |

Honest calibration: for a compiler whose clean-build baseline is already fast (Cranelift + DOD), **L1 covers most real-world pain for years**. L2 machinery should land *driven by features that hurt without it* (generics, `comptime`), not by anticipation.

## 3. How It Works (Mechanics)

The core model is a **demand-driven query system with dependency tracking**:

1. Every analysis is a query: `type_of(decl)`, `lowered_ir_of(fn)`, `effect_of(fn)`. Queries call queries.
2. Every query execution **records its dependency edges** (what it read) and caches its result, keyed by content hashes.
3. On recompile: hash the new source. A changed **body** invalidates only its own declaration; a changed **signature/interface** invalidates dependents. Unchanged inputs replay cached results.

The highest-leverage data-model decision is the **interface/body split**: callers depend on signatures, not bodies — and most real edits touch bodies, so most edits cascade nowhere.

## 4. Where Ryo Makes This Easy — and Hard

### 4.1 Easy (some of it accidental genius from Ownership Lite)

- **All core safety analyses are intraprocedural.** Ownership/borrow checking, projection rules P1–P6, freeze sets, eager destruction (P5), D5 scope-borrow verification — each checks one function body against fixed signatures. *No lifetimes means no cross-function borrow inference to invalidate.* Per-function caching is trivial. (Rust's borrowck, by contrast, depends on the lifetimed signatures of every callee — an edge Ryo's dependency graph simply does not have.)
- **No macros, no include model, explicit imports** — clean, shallow module graph; L1 caching is nearly free.
- **DOD architecture already points the right way:** interned symbols and index-keyed side tables (LLM instructions R1/R18) are natural cache keys; demand-driven laziness is already mandated (R4).
- **The v0.2 REPL is incremental's little sibling** — a persistent JIT session compiling new declarations against accumulated state. It forces session/dependency machinery early, on training wheels.

### 4.2 Hard (the two favorite features)

| Feature | Why it fights incremental | Mitigation |
|---------|---------------------------|------------|
| **`[yields]` effect inference** (concurrency §6.2) | Fixed-point over the **whole-program call graph**; one edit deep in a utility can flip an effect bit and invalidate every `with lock()` check upstream | Effect results are queries with dependency edges; re-run the fixed-point only over the affected strongly-connected component. Worst case: full re-run — budgeted at <10% of compile time, survivable |
| **`dyn` devirtualization** | Depends on the **whole-program reachable impl set**; adding one impl anywhere can change a decision in a distant file | Version the reachable set; devirtualization decisions keyed by `(call site, set version)` |
| **Cross-function inlining** (spec promises aggressive inlining) | Creates body-level edges: a callee body change must invalidate callers that inlined it | Record inlining decisions as dependency edges (standard technique) |
| **Monomorphized generics** (v0.3) | A generic body change invalidates all instantiations | Cache instantiations keyed by `(generic, type-args)`; body hash cascades only to instantiations |
| **`comptime`** (v0.3) | Arbitrary compile-time execution = arbitrary, invisible dependencies (file reads, helper results) | **Design constraint, written now:** comptime executes in a tracked sandbox where every input is recorded as a dependency edge. No untracked IO, ever (§6, H-5) |

## 5. Staged Roadmap

| Stage | Milestone | Deliverable |
|-------|-----------|-------------|
| **L1** package cache + fast clean builds | **v0.1–v0.2** | Go-model: hash per package, skip unchanged, relink. Plus the five hooks (§6) |
| Session-incremental | **v0.2** | REPL/JIT session contexts (already planned) — builds the machinery organically |
| **L2** analysis-level queries | **v0.3** | Minimal hand-rolled query caching, landing *with* generics + `comptime` — the first features that make compile times genuinely hurt and the ones most naturally cacheable |
| **L2** codegen caching | **v0.4+** | Per-function object cache keyed by `(body hash, dependency interface hashes)`; relink only |
| **L3** in-place patching | **Uncommitted** | Watch Zig (same compiler philosophy, years ahead); adopt proven techniques only if compile times become the product's top complaint |

## 6. The Five Hooks to Install Now (Cheap)

- **H-1 — Query-shaped analyses, enforced.** No hidden global reads; inputs flow through parameters (extends LLM instructions R4 from "lazy" to "lazy *and* dependency-explicit").
- **H-2 — Dependency edges recorded as a side effect of every query, from day one.** Cheap at analysis time; a rewrite to retrofit.
- **H-3 — Interface hashes separate from body hashes** in the module model (§3 — the single highest-leverage decision).
- **H-4 — Stable IDs across recompiles:** `DeclId`/`TypeId` carry a revision/generation counter (salsa's "revision" model) so cached results know what they're keyed against.
- **H-5 — `comptime` tracked-execution sandbox** specified in the v0.3 design: every input recorded, no untracked IO (§4.2).

## 7. Relationship to Existing Decisions

- **Base spec §16's "whole-program AOT" claims** need no retraction — whole-program *analysis results* are themselves cacheable queries with versioned inputs. Whole-program knowledge and incremental compilation coexist if dependencies are recorded; they conflict only if analyses read global state untracked (forbidden by H-1/H-2).
- **Concurrency plan §6.2's compile-time budget (<10%)** doubles as the worst-case fallback for effect-analysis invalidation (§4.2) — the mitigation is already costed.
- **LLM instructions:** H-1 tightens R4; H-2/H-4 extend R1/R18 (index-keyed side tables make invalidation surgical). An amendment to `ryo-compiler-llm-instructions.md` §1 folding in H-1…H-5 is the natural next step on approval.

## 8. Risks

| Risk | Mitigation |
|------|------------|
| Hooks bit-rot (recorded edges never consumed until v0.3) | A debug-mode `--dump-deps` flag from the start; CI asserts the dependency graph is well-formed on the test corpus |
| L2 scope creep — hand-rolled query system growing into a salsa clone | Charter it minimally: body/signature caching + SCC-scoped effect re-computation; nothing else without a measured need |
| `comptime` sandbox leaks (untracked IO slips in during v0.3 implementation) | Sandbox is in the *first* comptime milestone, not a follow-up; incremental soundness depends on it |

## 9. Open Questions

- **Q1 — Cache persistence:** on-disk query cache (rustc-style) vs. in-memory only for L2? Recommendation: in-memory at v0.3; on-disk evaluated with v0.4 codegen caching.
- **Q2 — Test story for incremental correctness:** replay-based tests (edit → recompile → assert identical binary behavior vs. clean build) should be part of L2's exit criteria; define the corpus then.
- **Q3 — Does L1 package caching hash *flags* too?** (profile, `--error-traces`, target.) Recommendation: yes — cache key includes the full configuration; a flag change is a full invalidation of affected packages.

---

*End of Design Note*
